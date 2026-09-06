//! `POST /api/kb/{kb}/atlas/recompute` — v0.3 real impl.
//!
//! v0.1 emitted start + complete in the same tick. v0.3 spawns the
//! actual PCA + k-means work via `kb_core::atlas::recompute_for_kb`,
//! returns 202 + `{run, events}` immediately, and emits the SSE pair
//! around the spawned task. `complete`'s payload carries the
//! `RecomputeReport` fields so the SPA can render "<N> points in <K>
//! clusters" without an extra fetch.

use crate::middleware::error_to_problem_json;
use crate::state::{AtlasOverrides, AtlasPointsCache, EdgesCache, KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::events::EventBus;
use kb_core::ids::RunId;
use kb_core::paths::doc_rel_path;
use kb_core::procrustes;
use kb_core::storage::lance::DocSummary;
use kb_core::storage::sqlite::{AtlasFramePoint, AtlasFrameRow};
use kb_core::storage::StorageHandle;
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::Path as StdPath;
use std::sync::{Arc, Mutex};
use tracing::warn;

pub async fn recompute(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let artifact_count = ctx.storage.count_rows().await.unwrap_or(0);
    let run = RunId::new();

    // P3 — single-flight. The atlas layout is the most expensive per-kb
    // job (up to O(n²·d) UMAP below the cap); a concurrent recompute on
    // the same kb is wasted work that thrashes the storage actor with a
    // second full `list_embeddings` + `update_atlas`. Take the slot under
    // the lock AFTER the `count_rows` await, with no await between taking
    // it and the spawn that owns its reset — a dropped handler future
    // can't strand it. If one is already running, return 202 with ITS run
    // id so the caller tails the same `atlas.recompute.complete` event
    // rather than starting (and waiting on) a second layout.
    {
        let mut slot = ctx
            .atlas_recompute
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = slot.as_ref() {
            return (
                StatusCode::ACCEPTED,
                Json(json!({
                    "run": existing,
                    "events": format!("/api/events?filter=run:{existing}"),
                    "kb": kb_name.as_str(),
                    "status": "already-running",
                })),
            )
                .into_response();
        }
        *slot = Some(run.to_string());
    }

    ctx.bus.emit(
        "atlas.recompute.start",
        json!({
            "run": run.as_str(),
            "kb": kb_name.as_str(),
            "artifact_count": artifact_count,
        }),
    );

    // W1.B — the caller-supplied clock read for the c-TF-IDF label rows
    // this recompute also (re)writes; kept OUT of `kb_core::atlas` itself
    // so the deterministic layout/label compute path stays clock-free
    // (crates/kb-core/CLAUDE.md invariant #3).
    let computed_at = chrono::Utc::now().timestamp();

    // Spawn the actual work. The handler returns 202 immediately; the
    // client tails /api/events for `atlas.recompute.complete` to learn
    // outcome + duration. Failures emit `atlas.recompute.complete` with
    // an `error` field — matches the v0.1 schema's permissive shape. The
    // spawned task resets the single-flight flag via a drop-guard.
    spawn_recompute(
        ctx.storage.clone(),
        Arc::clone(&ctx.bus),
        kb_name.clone(),
        run.clone(),
        ctx.atlas,
        computed_at,
        Arc::clone(&ctx.atlas_recompute),
    );

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "run": run.to_string(),
            "events": format!("/api/events?filter=run:{run}"),
        })),
    )
        .into_response()
}

#[derive(Serialize)]
pub struct EdgeOut {
    pub src: String,
    pub dst: String,
}

#[derive(Serialize)]
pub struct EdgesResponse {
    pub edges: Vec<EdgeOut>,
}

/// Wave-2 — the `kind='link'` edge pairs for `ctx`, served from the
/// per-(kb, index-generation) memo when warm (invariant #15). On a
/// generation change (or first call) it runs the one edges-table scan and
/// publishes the snapshot, so concurrent reader opens share it instead of
/// each re-scanning the whole edges table on the single storage actor. The
/// generation bumps on edge mutations (`record_edges → bump_generation`), so
/// a hit at the same generation is always fresh. The `std::sync::Mutex`
/// guard is dropped before the `.await`.
async fn link_pairs_cached(
    ctx: &KbContext,
    generation: u64,
) -> Result<Arc<Vec<(String, String)>>, kb_core::Error> {
    {
        let guard = ctx.edges_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref() {
            if c.generation == generation {
                return Ok(Arc::clone(&c.pairs));
            }
        }
    } // release the std Mutex BEFORE the await below — never held across .await

    let pairs = Arc::new(ctx.storage.link_pairs().await?);

    {
        let mut guard = ctx.edges_cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(EdgesCache {
            generation,
            pairs: Arc::clone(&pairs),
        });
    }
    Ok(pairs)
}

/// Per-boot ETag salt. The index-generation counter is process-local (a
/// fresh actor starts at 0), so an ETag derived from the generation alone
/// could collide across daemon restarts — same number, different graph —
/// and wrongly `304` a client that cached the previous boot's response.
/// Salting with the boot's epoch-millis makes each boot's ETags distinct.
static ETAG_BOOT_SALT: std::sync::LazyLock<u64> = std::sync::LazyLock::new(|| {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
});

/// `GET /api/kb/{kb}/edges` — list every `kind = 'link'` edge in the kb.
/// Backs the SPA atlas view's curved-edge layer (and the reader inspector's
/// links panel), fetched on every artifact open — so the result is memoised
/// per index-generation and carries an `ETag` derived from that generation
/// (a repeat request with a matching `If-None-Match` gets `304` and
/// re-downloads nothing while the graph is unchanged).
pub async fn list_edges(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let generation = ctx.storage.index_generation();
    let etag = format!("\"edges-{}-{generation}\"", *ETAG_BOOT_SALT);
    // Conditional GET: unchanged graph ⇒ 304, no body, no scan.
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == etag)
        .unwrap_or(false)
    {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(v) = HeaderValue::from_str(&etag) {
            resp.headers_mut().insert(header::ETAG, v);
        }
        return resp;
    }

    match link_pairs_cached(ctx, generation).await {
        Ok(pairs) => {
            let edges = pairs
                .iter()
                .map(|(src, dst)| EdgeOut {
                    src: src.clone(),
                    dst: dst.clone(),
                })
                .collect();
            let mut resp = Json(EdgesResponse { edges }).into_response();
            if let Ok(v) = HeaderValue::from_str(&etag) {
                resp.headers_mut().insert(header::ETAG, v);
            }
            resp
        }
        Err(e) => error_to_problem_json(&e),
    }
}

/// M-a — the FULL-corpus atlas doc scan (`list_docs_with_atlas(u32::MAX)`)
/// for `ctx`, served from the per-(kb, index-generation) memo when warm
/// (invariant #15). Copies `link_pairs_cached` above VERBATIM as the
/// pattern: the `std::sync::Mutex` guard is dropped before the `.await`
/// below, and a hit requires the stored generation to equal the caller's
/// `generation` (the storage actor's monotonic counter, which bumps only
/// on row-set/edge mutations — so a hit is always fresh). Read-only: this
/// never calls `bump_generation`. `pub(crate)` — `routes::atlas_field`'s
/// `disagreement` route reuses this SAME memo so the operator-field overlay
/// scores against the exact coordinates the map itself is drawing.
pub(crate) async fn atlas_docs_cached(
    ctx: &KbContext,
    generation: u64,
) -> Result<Arc<Vec<DocSummary>>, kb_core::Error> {
    {
        let guard = ctx
            .atlas_points_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref() {
            if c.generation == generation {
                return Ok(Arc::clone(&c.docs));
            }
        }
    } // release the std Mutex BEFORE the await below — never held across .await

    let docs = Arc::new(ctx.storage.list_docs_with_atlas(u32::MAX).await?);

    {
        let mut guard = ctx
            .atlas_points_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *guard = Some(AtlasPointsCache {
            generation,
            docs: Arc::clone(&docs),
        });
    }
    Ok(docs)
}

/// One point on the whole-corpus atlas map (`GET /atlas/points`, M-a).
/// Deliberately lean — the id/coords plus the minimal display fields the
/// SPA's `AtlasView` dot/tooltip actually reads (title, the reader-link
/// `source_relative`, and `kb_category` for tinting) — NOT the full
/// gallery-card `DocSummary` shape `?projection=atlas` on `/docs` returns.
/// `atlas_x`/`atlas_y`/`cluster` are `None` on a doc indexed before the
/// kb's first atlas recompute.
///
/// MI-W4.5 — `salience`/`pinned`/`forgotten`/`supersedes` are the same
/// memory metas `routes::memory::census` already surfaces, added here so
/// the atlas can double as a memory map. Populated ONLY when the kb is
/// memory-scoped (`ctx.memory_scope.is_some()`, the `[kb.*] memory_scope`
/// config gate — v0.9 M2) — an ordinary corpus's points are byte-unchanged
/// (every one of these four keys absent), even though `kb_salience`/
/// `kb_status`/`kb_supersedes` are always present in the underlying doc
/// scan (a doc could carry a stray memory meta without the kb being
/// memory-scoped; the atlas deliberately doesn't surface it there).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export, optional_fields))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasPoint {
    pub id: String,
    pub title: String,
    pub source_relative: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas_x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas_y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<i16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salience: Option<f32>,
    /// The raw `kb-decay` bucket ("slow" | "fast") — same categorical value
    /// `census`'s `decay_bucket` and `memory::rerank` read. Backs the
    /// atlas's decay-bucket color mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay_bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forgotten: Option<bool>,
    /// The artifact id this memory supersedes, when it does. The client
    /// draws one `AtlasEdge{src: id, dst: supersedes}` per point that has
    /// one, reusing the same curved-edge layer the `kind="link"` graph
    /// already renders (invariant #29) — no new visual machinery.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
}

/// One cluster's point count (M-a) — a server-side fold over `points` so
/// neither the CLI nor a future SPA caller has to recompute it locally.
/// Sorted by cluster id ascending; points with `cluster: None` (rows from
/// before the kb's first atlas recompute) are excluded, not bucketed under
/// a synthetic `0`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasClusterCount {
    pub cluster: i16,
    pub count: usize,
}

/// `GET /api/kb/{kb}/atlas/points` response (M-a).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasPointsResponse {
    pub points: Vec<AtlasPoint>,
    pub total: usize,
    pub clusters: Vec<AtlasClusterCount>,
}

/// Pure projection from the raw full-corpus doc scan to the wire shape —
/// pulled out of [`points`] so it's unit-testable without a storage actor.
/// `pinned` is `Some(&pinned_memories_set())` on a memory-scoped kb, `None`
/// otherwise — the single gate that decides whether ANY of the four memory
/// fields appear on the wire (see [`AtlasPoint`]'s doc comment).
fn build_atlas_points_response(
    docs: &[DocSummary],
    source_path: &StdPath,
    pinned: Option<&std::collections::HashSet<String>>,
) -> AtlasPointsResponse {
    let mut cluster_counts: BTreeMap<i16, usize> = BTreeMap::new();
    let points: Vec<AtlasPoint> = docs
        .iter()
        .map(|d| {
            if let Some(c) = d.atlas_cluster {
                *cluster_counts.entry(c).or_insert(0) += 1;
            }
            let (salience, decay_bucket, is_pinned, forgotten, supersedes) = match pinned {
                Some(p) => (
                    d.kb_salience,
                    d.kb_decay.clone(),
                    Some(p.contains(&d.id)),
                    Some(d.kb_status.as_deref() == Some("forgotten")),
                    d.kb_supersedes.clone(),
                ),
                None => (None, None, None, None, None),
            };
            AtlasPoint {
                id: d.id.clone(),
                title: d.title.clone(),
                source_relative: doc_rel_path(&d.path, source_path),
                atlas_x: d.atlas_x,
                atlas_y: d.atlas_y,
                cluster: d.atlas_cluster,
                kb_category: d.kb_category.clone(),
                salience,
                decay_bucket,
                pinned: is_pinned,
                forgotten,
                supersedes,
            }
        })
        .collect();
    let total = points.len();
    let clusters = cluster_counts
        .into_iter()
        .map(|(cluster, count)| AtlasClusterCount { cluster, count })
        .collect();
    AtlasPointsResponse {
        points,
        total,
        clusters,
    }
}

/// `GET /api/kb/{kb}/atlas/points` — the FULL-corpus atlas point set in ONE
/// storage scan (M-a). Fixes a measured bug, not just a gap: the SPA atlas
/// view today reads `?projection=atlas` off the paged `/docs` route, which
/// the gallery only ever calls with `limit=200` and never `loadMore`s for
/// the atlas branch — so on a corpus past 200 docs the map silently draws
/// a subset while its own status line claims the full count.
/// `?projection=atlas` also can't be widened past `MAX_ENVELOPE_LIMIT =
/// 500` and is served UNCACHED (a fresh `list_docs_with_atlas(u32::MAX)` +
/// `edge_counts()` on every call) — this route is the dedicated, memoised,
/// whole-kb read instead (see [`atlas_docs_cached`]). ETag'd like
/// `list_edges` above (boot-salted generation), so a repeat poll against an
/// unchanged graph is a bare `304` — **plus** (memory-scoped kbs only, MI-
/// W4.5 review fix) a hash of the current pinned-memory id set, since
/// pin/unpin never bumps `index_generation` (invariant #15 — it's not a
/// row-set/edge mutation) and would otherwise 304 back a stale `pinned`
/// flag on every point.
pub async fn points(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    headers: HeaderMap,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let generation = ctx.storage.index_generation();

    // MI-W4.5(review fix) — the memory-metadata gate: only a memory-scoped
    // kb pays for the pinned-set lookup (a small sqlite scan, not memoised
    // like `docs` below). Fetched BEFORE the ETag/304 check (not after, as
    // the first ship had it) and folded into the ETag itself — `pinned`/
    // `unpinned` (`StorageMsg::PinnedMemoryAdd`/`Remove`) writes only to the
    // `pinned_memories` sqlite table, which is NOT a row-set/edge mutation,
    // so it correctly does NOT bump `index_generation` (invariant #15). But
    // that means `generation` alone is stale-blind to pin state: without
    // this fold, a pin/unpin would 304 back the pre-pin snapshot to any
    // client polling with `If-None-Match` until the corpus ALSO happened to
    // change for an unrelated reason. Bumping the shared generation counter
    // instead was the other option, but that's the more expensive/wrong
    // choice — it would invalidate the gallery row-set memo and the
    // atlas-points doc-scan memo (`atlas_docs_cached`) on every pin/unpin
    // too, for state neither of them reads. Folding a hash of the pinned id
    // set into this route's OWN ETag is the cheaper, correctly-scoped fix:
    // it costs one extra sqlite scan per request for memory-scoped kbs
    // (small; same cost the body-building fetch below already paid on every
    // cache MISS) and touches no other cache's invalidation.
    let pinned = if ctx.memory_scope.is_some() {
        Some(ctx.storage.pinned_memories_set().await.unwrap_or_default())
    } else {
        None
    };
    let etag = match &pinned {
        Some(set) => {
            let mut sorted: Vec<&str> = set.iter().map(String::as_str).collect();
            sorted.sort_unstable();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            for id in &sorted {
                id.hash(&mut hasher);
            }
            format!(
                "\"atlas-points-{}-{generation}-p{:016x}\"",
                *ETAG_BOOT_SALT,
                hasher.finish()
            )
        }
        None => format!("\"atlas-points-{}-{generation}\"", *ETAG_BOOT_SALT),
    };
    // Conditional GET: unchanged corpus AND unchanged pin set ⇒ 304, no body,
    // no scan.
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == etag)
        .unwrap_or(false)
    {
        let mut resp = StatusCode::NOT_MODIFIED.into_response();
        if let Ok(v) = HeaderValue::from_str(&etag) {
            resp.headers_mut().insert(header::ETAG, v);
        }
        return resp;
    }

    let docs = match atlas_docs_cached(ctx, generation).await {
        Ok(d) => d,
        Err(e) => return error_to_problem_json(&e),
    };

    let body = build_atlas_points_response(&docs, &ctx.source_path, pinned.as_ref());
    let mut resp = Json(body).into_response();
    if let Ok(v) = HeaderValue::from_str(&etag) {
        resp.headers_mut().insert(header::ETAG, v);
    }
    resp
}

// --- Atlas time-lapse history (W3 T-b, V0028) --------------------------
//
// `GET /atlas/history` (the frame list) and `GET /atlas/history/{id}` (one
// frame's points, server-side Procrustes-aligned against another frame) —
// read the corpus time-lapse `record_atlas_snapshot`
// (`kb_core::atlas`) writes on every recompute/recluster. Plain reads, no
// rate-limit bucket, same posture as `labels`/`points`/`similar` above:
// small, bounded sqlite scans (at most `DEFAULT_ATLAS_FRAMES_KEEP = 24`
// frame rows), never a lance scan.
//
// FRAMES START EMPTY: a kb that has never recomputed its atlas (or one
// upgraded onto V0028 with no recompute since) has zero rows in
// `atlas_snapshots`. That is the honest state, not an error — `history_list`
// answers 200 with an empty `frames` array, never a 404; `history_show`
// still 404s (there is no frame `{id}` to show), but the CLI's empty-state
// message is what tells the operator *why* (see `kb-cli`'s
// `commands::atlas::history`) — and now also points at
// `POST /atlas/history/backfill` (W3 T-d, below), which can seed the
// timeline with RECONSTRUCTED frames. Reconstructed ≠ recorded: see that
// section's header before you treat a backfilled frame as history.

/// A frame beyond this cap would mean `DEFAULT_ATLAS_FRAMES_KEEP` grew far
/// past its documented 24 — this is deliberately generous headroom, not a
/// tuned value, so the route never needs to change if that constant does.
const ATLAS_HISTORY_LIST_CAP: u32 = 500;

/// One `atlas_snapshots` row on the wire — the metadata half of a frame,
/// with no points (`history_list`'s list stays cheap; `history_show`'s
/// `frame`/`align_to` fields reuse this same shape).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct AtlasFrameOut {
    pub id: i64,
    pub created_at_unix: i64,
    pub point_count: i64,
    /// NOT comparable across frames — `kmeans_lloyd` reseeds by array
    /// position, so cluster ids renumber between recomputes (see
    /// `kb_core::storage::sqlite::AtlasFramePoint`'s doc comment).
    pub cluster_count: i64,
    /// `"umap"` | `"pca"` | `"recluster"`.
    pub layout: String,
    /// `"recorded"` (written by the recompute/recluster that produced these
    /// coordinates — first-hand) | `"reconstructed"` (W3 T-d: written by
    /// `POST /atlas/history/backfill`, which lays TODAY's embeddings out over
    /// the docs that existed at a past `mtime_unix` cut point. NOT history —
    /// kb retains no past layout or embedding; see
    /// `kb_core::storage::sqlite::FrameProvenance`).
    /// Every frame the SPA/CLI show MUST label this — a time-lapse that
    /// can't tell first-hand from reconstructed history is a lie by omission.
    pub provenance: String,
}

impl From<&AtlasFrameRow> for AtlasFrameOut {
    fn from(r: &AtlasFrameRow) -> Self {
        AtlasFrameOut {
            id: r.id,
            created_at_unix: r.created_at_unix,
            point_count: r.point_count,
            cluster_count: r.cluster_count,
            layout: r.layout.clone(),
            provenance: r.provenance.clone(),
        }
    }
}

/// `GET /api/kb/{kb}/atlas/history` response.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasHistoryResponse {
    /// Newest first. Empty on a kb with no recorded frames yet — the
    /// honest starting state, never a 404 (see the module doc above).
    pub frames: Vec<AtlasFrameOut>,
}

/// `GET /api/kb/{kb}/atlas/history` — the time-lapse frame list, newest
/// first. See the module doc above for the empty-corpus honesty rule.
pub async fn history_list(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let frames = match ctx.storage.atlas_frames(ATLAS_HISTORY_LIST_CAP).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    Json(AtlasHistoryResponse {
        frames: frames.iter().map(AtlasFrameOut::from).collect(),
    })
    .into_response()
}

/// `?align_to=<frame-id>` on `GET /atlas/history/{id}`. Defaults to the
/// newest frame when absent — the common case ("how does this old frame
/// compare to where the map is now").
#[derive(Debug, Deserialize, Default)]
pub struct AtlasHistoryShowQuery {
    pub align_to: Option<i64>,
}

/// One frame's point on the wire, post-alignment.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasFramePointOut {
    pub artifact_id: String,
    /// Aligned into `align_to`'s coordinate frame (see [`AtlasFrameShowResponse::alignment`]).
    pub x: f32,
    pub y: f32,
    /// This frame's OWN cluster assignment — never remapped, and not
    /// comparable to `align_to`'s cluster ids (see [`AtlasFrameOut::cluster_count`]).
    pub cluster: i16,
}

/// The fitted Procrustes transform, on the wire (mirrors
/// `kb_core::procrustes::Transform`, flattened — see that module's doc
/// comment for why this is a closed-form `+ - * / sqrt` fit with no
/// `atan2`/`sin`/`cos`, and therefore bit-identical across machines/libcs).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasAlignmentOut {
    /// `cosθ`, obtained directly as `a/r` — never via an angle.
    pub cos: f32,
    /// `sinθ`, obtained directly as `b/r` — never via an angle.
    pub sin: f32,
    pub scale: f32,
    pub reflect: bool,
    pub from_centroid_x: f32,
    pub from_centroid_y: f32,
    pub to_centroid_x: f32,
    pub to_centroid_y: f32,
}

impl From<procrustes::Transform> for AtlasAlignmentOut {
    fn from(t: procrustes::Transform) -> Self {
        AtlasAlignmentOut {
            cos: t.cos,
            sin: t.sin,
            scale: t.scale,
            reflect: t.reflect,
            from_centroid_x: t.from_centroid.0,
            from_centroid_y: t.from_centroid.1,
            to_centroid_x: t.to_centroid.0,
            to_centroid_y: t.to_centroid.1,
        }
    }
}

/// `GET /api/kb/{kb}/atlas/history/{id}` response.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasFrameShowResponse {
    pub frame: AtlasFrameOut,
    pub align_to: AtlasFrameOut,
    /// EVERY point of `frame`, aligned into `align_to`'s coordinate space —
    /// not just the ids the two frames have in common (see
    /// [`fit_frame_alignment`]'s doc comment: the fit is computed over the
    /// overlap, then applied to the whole frame, since the transform is a
    /// single global similarity, not a per-point correspondence).
    pub points: Vec<AtlasFramePointOut>,
    pub alignment: AtlasAlignmentOut,
    /// Sum of squared distances between the aligned overlap and
    /// `align_to`'s points, over the `matched` ids below. **This is
    /// expected to be nonzero even when the underlying geometry is
    /// identical**: `kb_core::atlas`'s `normalise_to_unit` min-max-scales x
    /// and y INDEPENDENTLY, an anisotropic stretch a similarity transform
    /// (uniform scale + rotation + translation ± reflection) cannot fully
    /// undo by construction. Do not read a nonzero residual here as a bug —
    /// see `kb_core::procrustes`'s module doc "Honesty note" for the full
    /// argument, and do not "fix" this by fitting a shearing/affine
    /// transform instead (that would happily collapse a frame onto a line
    /// to minimise the number).
    pub residual: f32,
    /// How many artifact ids appeared in BOTH `frame` and `align_to` and
    /// therefore contributed to the fit + `residual`. `0` when the two
    /// frames share no artifacts (or `align_to == frame` on a frame with no
    /// points) — the alignment then falls back to
    /// [`procrustes::Transform::IDENTITY`] per `procrustes::align`'s
    /// documented degenerate-input behaviour.
    pub matched: usize,
}

/// Id-joined Procrustes fit between two frames' points.
///
/// `procrustes::align` pairs its two inputs **by index** (see that
/// function's doc comment) — it has no idea these are artifact ids, so a
/// caller MUST id-join before calling it, exactly as
/// `kb_core::atlas_field::joined_pairs`/`disagreement` do for the
/// machine/operator field overlay. Two atlas frames are not guaranteed to
/// name the same artifacts (docs get added/removed between recomputes) or
/// to list them in the same order, so this builds the `from`/`to` vectors
/// from the **BTreeMap-ordered intersection** of artifact ids — a
/// deterministic order independent of either frame's own row order (which
/// happens to already be `artifact_id`-sorted per
/// `Db::atlas_frame_points`'s `ORDER BY`, but this function doesn't rely on
/// that, only on the join itself being order-independent).
///
/// Returns `(fitted transform, residual over the overlap, overlap size)`.
fn fit_frame_alignment(
    frame: &[AtlasFramePoint],
    align_to: &[AtlasFramePoint],
) -> (procrustes::Transform, f32, usize) {
    let align_by_id: BTreeMap<&str, (f32, f32)> = align_to
        .iter()
        .map(|p| (p.artifact_id.as_str(), (p.x, p.y)))
        .collect();
    let mut from: Vec<(f32, f32)> = Vec::new();
    let mut to: Vec<(f32, f32)> = Vec::new();
    // BTreeMap-ordered walk of `frame`'s OWN ids (not `align_by_id`'s) so
    // the pairing order is a deterministic function of `frame` alone —
    // matches at most once each (duplicate artifact ids within one frame
    // can't happen: `atlas_snapshot_points`' primary key is
    // `(snapshot_id, artifact_id)`).
    let frame_by_id: BTreeMap<&str, (f32, f32)> = frame
        .iter()
        .map(|p| (p.artifact_id.as_str(), (p.x, p.y)))
        .collect();
    for (id, &f) in &frame_by_id {
        if let Some(&t) = align_by_id.get(id) {
            from.push(f);
            to.push(t);
        }
    }
    let matched = from.len();
    let transform = procrustes::align(&from, &to);
    let residual = procrustes::residual(&transform, &from, &to);
    (transform, residual, matched)
}

/// `GET /api/kb/{kb}/atlas/history/{id}` — one time-lapse frame's points,
/// Procrustes-aligned SERVER-SIDE against `align_to` (query param, default:
/// the newest frame). Doing the fit here — once — is the point: the CLI and
/// the SPA then agree on the SAME aligned coordinates + residual byte for
/// byte, rather than each re-implementing the alignment client-side and
/// risking the two silently drifting apart.
///
/// 404s when `{id}` (or an explicit `?align_to=`) doesn't name a stored
/// frame. Both metadata rows come from the SAME `atlas_frames` list call
/// (bounded at `ATLAS_HISTORY_LIST_CAP`), so this is one small sqlite scan
/// plus up to two `atlas_snapshot_points` reads (one, if `align_to == id`).
pub async fn history_show(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(q): Query<AtlasHistoryShowQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Ok(frame_id) = id.parse::<i64>() else {
        return (StatusCode::BAD_REQUEST, "atlas frame id must be an integer").into_response();
    };

    let frames = match ctx.storage.atlas_frames(ATLAS_HISTORY_LIST_CAP).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let Some(frame_row) = frames.iter().find(|f| f.id == frame_id) else {
        return (
            StatusCode::NOT_FOUND,
            format!("atlas frame {frame_id} not found"),
        )
            .into_response();
    };
    // `frames` is non-empty (it contains `frame_row`), so `frames[0]` — the
    // newest, per `Db::atlas_frames`' `ORDER BY created_at_unix DESC` — is a
    // safe default.
    let align_to_id = q.align_to.unwrap_or(frames[0].id);
    let Some(align_to_row) = frames.iter().find(|f| f.id == align_to_id) else {
        return (
            StatusCode::NOT_FOUND,
            format!("align_to atlas frame {align_to_id} not found"),
        )
            .into_response();
    };

    let frame_points = match ctx.storage.atlas_frame_points(frame_id).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    // Self-alignment (the default when the requested frame IS the newest)
    // reuses the same points instead of a second identical fetch.
    let align_points = if align_to_id == frame_id {
        frame_points.clone()
    } else {
        match ctx.storage.atlas_frame_points(align_to_id).await {
            Ok(v) => v,
            Err(e) => return error_to_problem_json(&e),
        }
    };

    let (transform, residual, matched) = fit_frame_alignment(&frame_points, &align_points);
    // Apply the fitted transform to EVERY point of `frame`, not just the
    // overlap `fit_frame_alignment` fit against — see
    // `AtlasFrameShowResponse::points`'s doc comment.
    let raw: Vec<(f32, f32)> = frame_points.iter().map(|p| (p.x, p.y)).collect();
    let aligned = procrustes::apply(&transform, &raw);
    let points: Vec<AtlasFramePointOut> = frame_points
        .iter()
        .zip(aligned)
        .map(|(p, (x, y))| AtlasFramePointOut {
            artifact_id: p.artifact_id.clone(),
            x,
            y,
            cluster: p.cluster,
        })
        .collect();

    Json(AtlasFrameShowResponse {
        frame: AtlasFrameOut::from(frame_row),
        align_to: AtlasFrameOut::from(align_to_row),
        points,
        alignment: AtlasAlignmentOut::from(transform),
        residual,
        matched,
    })
    .into_response()
}

/// `?keep=<n>` on `POST /atlas/history/prune` — REQUIRED (unlike the
/// insert-path auto-prune's `DEFAULT_ATLAS_FRAMES_KEEP`, an explicit
/// operator prune has no sensible implicit default: the operator reaches
/// for this route specifically to choose a retention count).
#[derive(Debug, Deserialize, Default)]
pub struct AtlasHistoryPruneQuery {
    pub keep: Option<u32>,
}

/// `POST /api/kb/{kb}/atlas/history/prune` response.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasPruneResponse {
    /// Frames actually deleted (their points cascade) — the honest count,
    /// not `keep` itself: a kb with fewer than `keep` frames removes zero.
    pub removed: usize,
}

/// `POST /api/kb/{kb}/atlas/history/prune?keep=N` — explicit operator
/// retention: drop all but the newest `keep` frames. The insert path
/// already self-prunes to `DEFAULT_ATLAS_FRAMES_KEEP` (24) on every
/// recompute/recluster; this route exists for an operator who wants a
/// tighter bound right now, without waiting for that many more recomputes
/// to happen. `400`s when `?keep=` is missing — see
/// [`AtlasHistoryPruneQuery`]'s doc comment on why there is no implicit
/// default here.
pub async fn history_prune(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(q): Query<AtlasHistoryPruneQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(keep) = q.keep else {
        return (StatusCode::BAD_REQUEST, "?keep=<n> is required").into_response();
    };
    match ctx.storage.atlas_frames_prune(keep).await {
        Ok(removed) => Json(AtlasPruneResponse { removed }).into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

// --- W3 T-d: the RECONSTRUCTED backfill --------------------------------
//
// `POST /atlas/history/backfill?frames=N` seeds an empty (or thin) time-lapse
// by laying out, for each of N evenly-spaced `mtime_unix` cut points, the
// docs that EXISTED at that time — using TODAY's embeddings — and storing
// each result as a `provenance = 'reconstructed'` frame.
//
// **This is a RECONSTRUCTION, not history.** True historical layouts are
// impossible: nothing retains a past layout or a past embedding (the lance
// row holds only the CURRENT atlas_x/atlas_y/atlas_cluster, overwritten
// wholesale by `update_atlas`). A reconstructed frame answers "where would
// these docs have sat, if I had run the atlas then, knowing what I know
// now" — genuinely useful, and genuinely not the same thing. Every surface
// says the word: the row's `provenance`, this response's `provenance` +
// `note`, `kb atlas backfill`/`history`/`show`, and the SPA scrubber.
//
// Posture copied from `recompute` above, deliberately: it is the same class
// of expensive whole-corpus write (N layouts instead of one). It SHARES
// `recompute`'s single-flight slot — a backfill racing a recompute would
// have both hammering `list_embeddings` on one storage actor — returns 202
// + a run id, emits SSE progress, and rides the same `atlas_limiter` bucket
// (`router.rs`'s `atlas_routes`) so it can't be trivially spammed.

/// `?frames=<n>` on the backfill route. Optional; defaults to
/// [`BACKFILL_DEFAULT_FRAMES`] and 400s above [`BACKFILL_MAX_FRAMES`]
/// (never a silent clamp — the operator asked for a specific spend).
#[derive(Debug, Deserialize, Default)]
pub struct AtlasBackfillQuery {
    pub frames: Option<u32>,
}

/// Default cut-point count — enough to see drift, cheap enough to be a
/// reasonable default spend on a cold kb.
const BACKFILL_DEFAULT_FRAMES: u32 = 8;
/// Hard cap. Each frame is a FULL layout pass over its subset, so this is a
/// multiplier on the single most expensive per-kb job.
const BACKFILL_MAX_FRAMES: u32 = 12;

/// One planned cut point on the wire — what the CLI prints BEFORE the work
/// runs. `doc_count` counts by `mtime_unix` alone; the frame finally written
/// can be smaller (only docs that also carry an embedding can be laid out),
/// which is why the per-frame SSE reports its own point count.
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasBackfillCut {
    pub cut_unix: i64,
    pub doc_count: usize,
}

/// `POST /api/kb/{kb}/atlas/history/backfill` response (202).
///
/// Deliberately NOT ts-exported: the SPA has no backfill surface (this is an
/// operator/CLI action — putting an expensive whole-corpus relayout behind a
/// stray click is wrong), so there is no TS consumer to keep in lock-step.
#[derive(Debug, Serialize, PartialEq)]
pub struct AtlasBackfillResponse {
    pub run: String,
    pub events: String,
    pub kb: String,
    /// `"started"` | `"already-running"` (another recompute/backfill owns
    /// the kb's single-flight slot; `cuts` is then empty and `run` is THAT
    /// run's id).
    pub status: String,
    /// Always `"reconstructed"` — the provenance every frame this run writes
    /// will carry. Stated on the wire so no client has to infer it.
    pub provenance: String,
    /// The plan, oldest cut first. Empty when the corpus has no usable
    /// `mtime_unix` at all, or when `status == "already-running"`.
    pub cuts: Vec<AtlasBackfillCut>,
    /// Docs excluded from the time axis because they carry no `mtime_unix`
    /// (they appear in no frame — never silently folded into the oldest cut).
    pub docs_without_mtime: usize,
    /// The honesty sentence, verbatim, so every client repeats the SAME
    /// claim rather than paraphrasing it into something stronger.
    pub note: String,
}

/// The one sentence every backfill surface repeats.
pub const BACKFILL_NOTE: &str = "reconstructed frames use TODAY's embeddings over the docs that \
existed at each cut point (by mtime) — they are not recorded history; nothing in kb retains a past \
layout or a past embedding";

/// Build the plan response (pure — no storage, no clock), so the cut
/// arithmetic + the honesty fields are unit-testable without a daemon.
fn build_backfill_plan(
    kb: &str,
    run: &str,
    docs: &[DocSummary],
    frames: u32,
) -> (
    Vec<kb_core::atlas::ReconstructionCut>,
    AtlasBackfillResponse,
) {
    // W3 T-d — the time axis is `mtime_unix`, NEVER `indexed_at_unix`: the
    // latter is `unix_now()` at index time, so one `kb reindex` stamps the
    // whole corpus with today and every cut point would return everything.
    let mtimes: Vec<i64> = docs.iter().filter_map(|d| d.mtime_unix).collect();
    let docs_without_mtime = docs.len() - mtimes.len();
    let cuts = kb_core::atlas::plan_reconstruction(&mtimes, frames as usize);
    let resp = AtlasBackfillResponse {
        run: run.to_string(),
        events: format!("/api/events?filter=run:{run}"),
        kb: kb.to_string(),
        status: "started".to_string(),
        provenance: kb_core::storage::sqlite::FrameProvenance::Reconstructed
            .as_str()
            .to_string(),
        cuts: cuts
            .iter()
            .map(|c| AtlasBackfillCut {
                cut_unix: c.cut_unix,
                doc_count: c.doc_count,
            })
            .collect(),
        docs_without_mtime,
        note: BACKFILL_NOTE.to_string(),
    };
    (cuts, resp)
}

/// `POST /api/kb/{kb}/atlas/history/backfill?frames=N` — reconstruct up to
/// N back-dated time-lapse frames. See the module section above for what a
/// reconstructed frame does and does not claim.
pub async fn history_backfill(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(q): Query<AtlasBackfillQuery>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let frames = q.frames.unwrap_or(BACKFILL_DEFAULT_FRAMES);
    if frames == 0 || frames > BACKFILL_MAX_FRAMES {
        return (
            StatusCode::BAD_REQUEST,
            format!("?frames= must be between 1 and {BACKFILL_MAX_FRAMES}"),
        )
            .into_response();
    }

    // The plan is computed SYNCHRONOUSLY, before the spawn, so the 202 body
    // can tell the operator what is about to happen (the CLI prints it
    // before waiting). It rides the same per-generation memo the map itself
    // reads (invariant #15) — no extra full scan.
    let generation = ctx.storage.index_generation();
    let docs = match atlas_docs_cached(ctx, generation).await {
        Ok(d) => d,
        Err(e) => return error_to_problem_json(&e),
    };
    let run = RunId::new();
    let (cuts, mut plan) = build_backfill_plan(kb_name.as_str(), run.as_str(), &docs, frames);

    // Single-flight, SHARED with `recompute` (see the module section) —
    // taken after every await above, with none between taking it and the
    // spawn that owns its reset.
    {
        let mut slot = ctx
            .atlas_recompute
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = slot.as_ref() {
            plan.run = existing.clone();
            plan.events = format!("/api/events?filter=run:{existing}");
            plan.status = "already-running".to_string();
            plan.cuts = Vec::new();
            return (StatusCode::ACCEPTED, Json(plan)).into_response();
        }
        *slot = Some(run.to_string());
    }

    // The id→mtime join the reconstruction filters each cut by. Built here
    // (from the memo) rather than inside kb-core so the compute path takes
    // no second full scan.
    let mtime_by_id: std::collections::HashMap<String, i64> = docs
        .iter()
        .filter_map(|d| d.mtime_unix.map(|m| (d.id.clone(), m)))
        .collect();

    spawn_backfill(
        ctx.storage.clone(),
        Arc::clone(&ctx.bus),
        kb_name.clone(),
        run.clone(),
        ctx.atlas,
        mtime_by_id,
        cuts,
        Arc::clone(&ctx.atlas_recompute),
    );

    (StatusCode::ACCEPTED, Json(plan)).into_response()
}

#[allow(clippy::too_many_arguments)]
fn spawn_backfill(
    storage: StorageHandle,
    bus: Arc<EventBus>,
    kb: KbName,
    run: RunId,
    atlas: AtlasOverrides,
    mtime_by_id: std::collections::HashMap<String, i64>,
    cuts: Vec<kb_core::atlas::ReconstructionCut>,
    recompute_slot: Arc<Mutex<Option<String>>>,
) {
    tokio::spawn(async move {
        // Same drop-guard discipline as `spawn_recompute` — a panic can
        // never wedge the kb out of all future recomputes/backfills.
        struct ResetOnDrop(Arc<Mutex<Option<String>>>);
        impl Drop for ResetOnDrop {
            fn drop(&mut self) {
                if let Ok(mut slot) = self.0.lock() {
                    *slot = None;
                }
            }
        }
        let _guard = ResetOnDrop(recompute_slot);

        bus.emit(
            "atlas.backfill.start",
            json!({
                "run": run.as_str(),
                "kb": kb.as_str(),
                "frames": cuts.len(),
                "provenance": "reconstructed",
            }),
        );

        let result = kb_core::atlas::backfill_reconstructed_frames(
            &storage,
            &mtime_by_id,
            &cuts,
            atlas.k,
            atlas.layout.to_kind(),
            |frame| {
                // Per-frame progress on the EXISTING key, so the SPA's
                // time-lapse invalidation bridge picks a backfill up exactly
                // as it picks up a recompute's frame.
                if let kb_core::atlas::FrameOutcome::Written { frame_id } = frame.outcome {
                    bus.emit(
                        "atlas.snapshot.recorded",
                        json!({
                            "kb": kb.as_str(),
                            "id": frame_id,
                            "points": frame.points,
                        }),
                    );
                }
            },
        )
        .await;

        match result {
            Ok(report) => {
                bus.emit(
                    "atlas.backfill.complete",
                    json!({
                        "run": run.as_str(),
                        "kb": kb.as_str(),
                        "written": report.written,
                        "skipped": report.skipped,
                        "duration_ms": report.duration_ms,
                        "provenance": "reconstructed",
                    }),
                );
            }
            Err(e) => {
                warn!(kb = %kb, run = %run, error = %e, "atlas backfill failed");
                bus.emit(
                    "atlas.backfill.complete",
                    json!({
                        "run": run.as_str(),
                        "kb": kb.as_str(),
                        "written": 0,
                        "skipped": 0,
                        "duration_ms": 0,
                        "error": e.to_string(),
                    }),
                );
            }
        }
    });
}

/// One ranked c-TF-IDF term (W1.B). Mirrors `kb_core::atlas_labels::TermScore`
/// on the wire.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct AtlasTermScore {
    pub term: String,
    pub tf: f64,
    pub ft: f64,
    pub score: f64,
}

/// Top terms for one atlas cluster.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct AtlasClusterLabels {
    pub cluster: i16,
    pub terms: Vec<AtlasTermScore>,
}

/// `GET /api/kb/{kb}/atlas/labels` response (W1.B).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct AtlasLabelsResponse {
    pub clusters: Vec<AtlasClusterLabels>,
    /// `A` — mean token count per cluster, shared by every term's
    /// `tf * ln(1 + A/ft)` decomposition. Recovered from a stored row
    /// rather than persisted separately (the `atlas_labels` table only
    /// carries the per-term decomposition) — see
    /// `kb_core::atlas_labels::recover_avg_tokens`. `0.0` when `clusters`
    /// is empty.
    pub avg_tokens: f64,
    /// Unix seconds this label set was (re)computed at. `None` when no
    /// recompute/recluster has ever run for this kb.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub computed_at: Option<i64>,
}

/// `GET /api/kb/{kb}/atlas/labels` — deterministic c-TF-IDF cluster labels
/// (W1.B), refreshed on every atlas recompute/recluster
/// (`atlas::recompute_for_kb_with`/`recluster_for_kb_with`). Read-only,
/// no memoization needed (one small sqlite table scan); a kb that hasn't
/// recomputed yet — or whose corpus is too small to cluster — serves an
/// empty `clusters` list rather than a 404, so the SPA atlas view can fall
/// back to its existing client-side dominant-tag label.
pub async fn labels(State(state): State<Arc<KbHandles>>, Path(kb): Path<String>) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let rows = match ctx.storage.atlas_labels().await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };

    let computed_at = rows.first().map(|r| r.computed_at);
    let avg_tokens = rows
        .first()
        .map(|r| kb_core::atlas_labels::recover_avg_tokens(r.tf, r.ft, r.score))
        .unwrap_or(0.0);

    // `Db::atlas_labels` already returns rows ordered `(cluster, rank)`, so
    // consecutive runs group into clusters without a HashMap (keeps score-
    // desc term order within each cluster instead of an incidental hash
    // order).
    let mut clusters: Vec<AtlasClusterLabels> = Vec::new();
    for row in rows {
        let term = AtlasTermScore {
            term: row.term,
            tf: row.tf,
            ft: row.ft,
            score: row.score,
        };
        match clusters.last_mut() {
            Some(c) if c.cluster == row.cluster => c.terms.push(term),
            _ => clusters.push(AtlasClusterLabels {
                cluster: row.cluster,
                terms: vec![term],
            }),
        }
    }

    Json(AtlasLabelsResponse {
        clusters,
        avg_tokens,
        computed_at,
    })
    .into_response()
}

/// One ranked true-neighbor (W2.3a) — embedding-space cosine similarity.
/// Distinct from `kb related` (the `kind='link'` wikilink/hub graph,
/// `GET /api/kb/{kb}/graph/{id}`) and from text-query recall (rank-position
/// × salience × decay, root invariant #10): this is pure vector-space
/// nearness. `atlas_x`/`atlas_y`/`cluster` ride along when the neighbor's
/// row has them (absent on a kb that hasn't run an atlas recompute yet, or
/// a row indexed before one) so the SPA can compare this REAL cosine score
/// against 2-D layout distance — the "is the map lying to you" honesty
/// check the AtlasInspector's neighbors rail wants.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export, optional_fields))]
#[derive(Debug, Serialize, PartialEq)]
pub struct SimilarOut {
    pub id: String,
    pub title: String,
    pub source_relative: String,
    /// Normalized dot product, computed HERE from the raw vectors — never
    /// lance's undecoded `_distance` column (see `vector_query`'s doc
    /// comment on why that column never reaches the wire).
    pub cosine: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas_x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas_y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster: Option<i16>,
}

/// `GET /api/kb/{kb}/atlas/similar/{id}` response (W2.3a).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export, optional_fields))]
#[derive(Debug, Serialize, PartialEq)]
pub struct SimilarResponse {
    pub neighbors: Vec<SimilarOut>,
    /// Honest-empty marker: `"no-embedding"` when the seed doc itself has no
    /// stored embedding (never indexed with one, or a kb with no
    /// `embedding_model` configured) — the caller gets a 200 with an empty
    /// list and a reason, not a 404 (the doc may well exist). Absent
    /// whenever `neighbors` came from a real vector query, even one that
    /// itself found zero other docs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `?limit=<n>` on the similar route. Default 8, capped at 24 — this backs
/// an inspector rail, not a page.
#[derive(Debug, Deserialize, Default)]
pub struct SimilarQuery {
    pub limit: Option<u32>,
}

const SIMILAR_DEFAULT_LIMIT: u32 = 8;
const SIMILAR_MAX_LIMIT: u32 = 24;

/// `GET /api/kb/{kb}/atlas/similar/{id}` — true (embedding-space) nearest
/// neighbors for one doc (W2.3a). Sibling of `labels` above: a plain read,
/// no rate-limit bucket (rides the base api tree, not `atlas_routes`'s
/// recompute/recluster limiter) — one `embedding_by_id` seek, one bounded
/// `vector_query`, then one `embeddings_by_ids` + one `get_by_ids` over the
/// small (`limit`-capped) neighbor set. Cosine is computed HERE from the
/// raw vectors so every neighbor carries a real, comparable score.
pub async fn similar(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(q): Query<SimilarQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let limit = q
        .limit
        .unwrap_or(SIMILAR_DEFAULT_LIMIT)
        .clamp(1, SIMILAR_MAX_LIMIT);

    let seed_vec = match ctx.storage.embedding_by_id(id.clone()).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let Some(seed_vec) = seed_vec else {
        return Json(SimilarResponse {
            neighbors: Vec::new(),
            reason: Some("no-embedding".to_string()),
        })
        .into_response();
    };

    // Over-fetch by one to survive dropping the self-hit — the seed doc is
    // always its own nearest neighbor at cosine 1.0.
    let hits = match ctx.storage.vector_query(seed_vec.clone(), limit + 1).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let hit_ids: Vec<String> = hits.into_iter().map(|h| h.id).collect();
    let neighbor_ids = drop_self_hit(hit_ids, &id, limit);
    if neighbor_ids.is_empty() {
        return Json(SimilarResponse {
            neighbors: Vec::new(),
            reason: None,
        })
        .into_response();
    }

    let embeds = match ctx.storage.embeddings_by_ids(neighbor_ids.clone()).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let embed_by_id: std::collections::HashMap<String, Vec<f32>> = embeds.into_iter().collect();

    // W2.3a — `get_by_ids`'s projection carries atlas_x/y/cluster (unlike
    // `vector_query`'s SEARCH_PROJECTION), so this one round-trip resolves
    // title/source-path/atlas coords for the whole (small) neighbor set.
    let docs = match ctx.storage.get_by_ids(neighbor_ids.clone()).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let doc_by_id: std::collections::HashMap<String, kb_core::storage::lance::DocSummary> =
        docs.into_iter().map(|d| (d.id.clone(), d)).collect();

    let mut neighbors: Vec<SimilarOut> = Vec::new();
    for nid in &neighbor_ids {
        // A neighbor missing from either lookup (rare TOCTOU — deleted
        // between the vector query and these two resolves) is simply
        // dropped rather than surfaced half-populated.
        let (Some(vec), Some(doc)) = (embed_by_id.get(nid), doc_by_id.get(nid)) else {
            continue;
        };
        neighbors.push(SimilarOut {
            id: nid.clone(),
            title: doc.title.clone(),
            source_relative: doc_rel_path(&doc.path, &ctx.source_path),
            cosine: cosine_similarity(&seed_vec, vec),
            atlas_x: doc.atlas_x,
            atlas_y: doc.atlas_y,
            cluster: doc.atlas_cluster,
        });
    }
    // Rank by OUR real cosine, descending — not lance's internal scan
    // order. Tie-break by id for determinism (mirrors `rank_sort`'s
    // tie-break discipline elsewhere in the storage layer).
    neighbors.sort_by(|a, b| {
        b.cosine
            .partial_cmp(&a.cosine)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    Json(SimilarResponse {
        neighbors,
        reason: None,
    })
    .into_response()
}

/// Pure, storage-free helper pulled out of `similar` so it's directly
/// unit-testable: drop the seed's own id from the `vector_query` hit ids
/// (its self-hit — a doc is always its own nearest neighbor at cosine 1.0)
/// and cap to `limit`. Order here is whatever `vector_query` returned;
/// `similar` re-sorts by its own real cosine afterward, so this fn's job
/// is purely "which ids survive", not final rank.
fn drop_self_hit(hit_ids: Vec<String>, seed_id: &str, limit: u32) -> Vec<String> {
    hit_ids
        .into_iter()
        .filter(|hid| hid != seed_id)
        .take(limit as usize)
        .collect()
}

/// Cosine similarity between two equal-length vectors — the neighbors
/// rail's one real score (never lance's undecoded `_distance`; see
/// `vector_query`'s doc comment). Returns `0.0` on a length mismatch or a
/// zero vector (the embedding model never emits an all-zero vector, but a
/// corrupt row theoretically could) rather than dividing by zero or
/// panicking on a shape mismatch.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// `?k=<n>` query param for the recluster route. Optional override on
/// the per-kb default (√n, capped at MAX_CLUSTERS). When omitted the
/// kb's resolved `AtlasOverrides.k` (from kb.toml) wins.
#[derive(Debug, Deserialize, Default)]
pub struct RecomputeQuery {
    #[serde(default)]
    pub k: Option<usize>,
}

/// `POST /api/kb/{kb}/atlas/recluster` — fast path that re-runs only
/// k-means on existing atlas coords. Mirrors `recompute` but emits
/// `atlas.recluster.{start,complete}` and never invokes UMAP/PCA.
///
/// Use case: tweak `?k=6` without paying the UMAP cost; the SPA's
/// dot positions don't move (no tween needed) but cluster colors
/// re-balance.
pub async fn recluster(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(q): Query<RecomputeQuery>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let run = RunId::new();
    let artifact_count = ctx.storage.count_rows().await.unwrap_or(0);

    ctx.bus.emit(
        "atlas.recluster.start",
        json!({
            "run": run.as_str(),
            "kb": kb_name.as_str(),
            "artifact_count": artifact_count,
        }),
    );

    // ?k= wins over the kb.toml default — operators reach for this
    // route precisely to override the per-kb setting.
    let k_override = q.k.or(ctx.atlas.k);
    // W1.B — see the matching comment in `recompute` above.
    let computed_at = chrono::Utc::now().timestamp();
    spawn_recluster(
        ctx.storage.clone(),
        Arc::clone(&ctx.bus),
        kb_name.clone(),
        run.clone(),
        k_override,
        computed_at,
    );

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "run": run.to_string(),
            "events": format!("/api/events?filter=run:{run}"),
        })),
    )
        .into_response()
}

fn spawn_recompute(
    storage: StorageHandle,
    bus: Arc<EventBus>,
    kb: KbName,
    run: RunId,
    atlas: AtlasOverrides,
    computed_at: i64,
    recompute_slot: Arc<Mutex<Option<String>>>,
) {
    tokio::spawn(async move {
        // Release the single-flight slot on completion OR panic — `Drop`
        // runs when the task future unwinds, so a failed/aborted recompute
        // can never wedge the kb out of all future recomputes. `if let Ok`
        // so a poisoned lock doesn't double-panic in drop.
        struct ResetOnDrop(Arc<Mutex<Option<String>>>);
        impl Drop for ResetOnDrop {
            fn drop(&mut self) {
                if let Ok(mut slot) = self.0.lock() {
                    *slot = None;
                }
            }
        }
        let _guard = ResetOnDrop(recompute_slot);

        // W3 T-b — the newest frame id BEFORE the recompute runs, so a
        // changed id afterward can only mean THIS call's
        // `record_atlas_snapshot` wrote it (see `emit_snapshot_recorded_if_new`).
        let before_frame_id = newest_frame_id(&storage).await;

        let result = kb_core::atlas::recompute_for_kb_with(
            &storage,
            atlas.k,
            atlas.layout.to_kind(),
            computed_at,
        )
        .await;
        match result {
            Ok(report) => {
                bus.emit(
                    "atlas.recompute.complete",
                    json!({
                        "run": run.as_str(),
                        "kb": kb.as_str(),
                        "points": report.points,
                        "clusters": report.clusters,
                        "duration_ms": report.duration_ms,
                    }),
                );
                emit_snapshot_recorded_if_new(&storage, &bus, &kb, before_frame_id).await;
            }
            Err(e) => {
                warn!(kb = %kb, run = %run, error = %e, "atlas recompute failed");
                bus.emit(
                    "atlas.recompute.complete",
                    json!({
                        "run": run.as_str(),
                        "kb": kb.as_str(),
                        "duration_ms": 0,
                        "error": e.to_string(),
                    }),
                );
            }
        }
    });
}

fn spawn_recluster(
    storage: StorageHandle,
    bus: Arc<EventBus>,
    kb: KbName,
    run: RunId,
    k_override: Option<usize>,
    computed_at: i64,
) {
    tokio::spawn(async move {
        // W3 T-b — see the matching comment in `spawn_recompute` above.
        // `recluster` has no P3 single-flight slot, so in principle a
        // concurrent recompute/recluster for the same kb could land its
        // frame in this window and this call would (mis)report THAT frame
        // as its own; acceptable for a best-effort SSE notification — same
        // posture `record_atlas_snapshot` itself takes (log-and-swallow).
        let before_frame_id = newest_frame_id(&storage).await;
        match kb_core::atlas::recluster_for_kb_with(&storage, k_override, computed_at).await {
            Ok(report) => {
                bus.emit(
                    "atlas.recluster.complete",
                    json!({
                        "run": run.as_str(),
                        "kb": kb.as_str(),
                        "points": report.points,
                        "clusters": report.clusters,
                        "duration_ms": report.duration_ms,
                    }),
                );
                emit_snapshot_recorded_if_new(&storage, &bus, &kb, before_frame_id).await;
            }
            Err(e) => {
                warn!(kb = %kb, run = %run, error = %e, "atlas recluster failed");
                bus.emit(
                    "atlas.recluster.complete",
                    json!({
                        "run": run.as_str(),
                        "kb": kb.as_str(),
                        "duration_ms": 0,
                        "error": e.to_string(),
                    }),
                );
            }
        }
    });
}

/// The current newest atlas frame's id, if any — one `atlas_frames(1)`
/// read. `None` on a storage error OR a kb with no frames yet; either way
/// the caller treats "no baseline" the same as "not this frame", which is
/// the conservative (never over-report) direction.
async fn newest_frame_id(storage: &StorageHandle) -> Option<i64> {
    storage
        .atlas_frames(1)
        .await
        .ok()
        .and_then(|v| v.into_iter().next())
        .map(|f| f.id)
}

/// W3 T-b — best-effort emit of `atlas.snapshot.recorded {kb, id, points}`
/// when a recompute/recluster ACTUALLY appended a new time-lapse frame
/// (V0028). `record_atlas_snapshot` (`kb_core::atlas`) is itself
/// best-effort and log-only — kb-core has no `EventBus` handle to emit
/// through (root invariant: no clock/no bus in the deterministic compute
/// path) — so detecting "did THIS call append a frame" happens here, by
/// comparing the newest frame's id before vs. after: frame ids are
/// `AUTOINCREMENT` and strictly increasing, and pruning only ever drops the
/// OLDEST frames (the newest one is never pruned — see
/// `prune_atlas_frames`'s `ORDER BY … DESC LIMIT keep`), so a changed
/// newest-id is unambiguous evidence a new row landed. `record_atlas_snapshot`'s
/// dedup means a recompute that reproduced bit-identical coordinates writes
/// NO new row, so this correctly emits nothing for a no-op recompute.
async fn emit_snapshot_recorded_if_new(
    storage: &StorageHandle,
    bus: &EventBus,
    kb: &KbName,
    before_frame_id: Option<i64>,
) {
    let Some(after) = storage
        .atlas_frames(1)
        .await
        .ok()
        .and_then(|v| v.into_iter().next())
    else {
        return;
    };
    if Some(after.id) != before_frame_id {
        bus.emit(
            "atlas.snapshot.recorded",
            json!({
                "kb": kb.as_str(),
                "id": after.id,
                "points": after.point_count,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- M-a: build_atlas_points_response — pure projection ----------------

    fn point_doc(id: &str, x: Option<f32>, y: Option<f32>, cluster: Option<i16>) -> DocSummary {
        DocSummary {
            id: id.into(),
            title: format!("Doc {id}"),
            path: format!("/root/{id}.html"),
            atlas_x: x,
            atlas_y: y,
            atlas_cluster: cluster,
            kb_category: Some("note".into()),
            ..Default::default()
        }
    }

    #[test]
    fn build_atlas_points_response_projects_every_doc_and_counts_clusters() {
        let docs = vec![
            point_doc("a", Some(1.0), Some(2.0), Some(0)),
            point_doc("b", Some(3.0), Some(4.0), Some(1)),
            point_doc("c", Some(5.0), Some(6.0), Some(0)),
            // A row indexed before the kb's first atlas recompute: no
            // coords/cluster yet, but still a full-corpus point (the whole
            // reason this route exists — the paged gallery would drop it
            // past its 200-doc default limit on a large corpus).
            point_doc("d", None, None, None),
        ];
        let resp = build_atlas_points_response(&docs, StdPath::new("/root"), None);

        assert_eq!(resp.total, 4);
        assert_eq!(resp.points.len(), 4);
        assert_eq!(resp.points[0].id, "a");
        assert_eq!(resp.points[0].source_relative, "a.html");
        assert_eq!(resp.points[3].atlas_x, None);
        assert_eq!(resp.points[3].cluster, None);

        // Cluster counts: cluster 0 has 2 points, cluster 1 has 1; the
        // uncomputed doc ("d", cluster None) contributes to NEITHER bucket
        // (never a synthetic "cluster 0"), and buckets are sorted by
        // cluster id ascending.
        assert_eq!(
            resp.clusters,
            vec![
                AtlasClusterCount {
                    cluster: 0,
                    count: 2
                },
                AtlasClusterCount {
                    cluster: 1,
                    count: 1
                },
            ]
        );
    }

    #[test]
    fn build_atlas_points_response_empty_corpus_is_honest_empty() {
        let resp = build_atlas_points_response(&[], StdPath::new("/root"), None);
        assert_eq!(resp.total, 0);
        assert!(resp.points.is_empty());
        assert!(resp.clusters.is_empty());
    }

    #[test]
    fn atlas_point_omits_absent_optional_fields_on_the_wire() {
        let p = AtlasPoint {
            id: "a".into(),
            title: "Doc A".into(),
            source_relative: "a.html".into(),
            atlas_x: None,
            atlas_y: None,
            cluster: None,
            kb_category: None,
            salience: None,
            decay_bucket: None,
            pinned: None,
            forgotten: None,
            supersedes: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("atlas_x"));
        assert!(!obj.contains_key("atlas_y"));
        assert!(!obj.contains_key("cluster"));
        assert!(!obj.contains_key("kb_category"));
        assert!(!obj.contains_key("salience"));
        assert!(!obj.contains_key("decay_bucket"));
        assert!(!obj.contains_key("pinned"));
        assert!(!obj.contains_key("forgotten"));
        assert!(!obj.contains_key("supersedes"));
    }

    // --- MI-W4.5: the memory-scope gate --------------------------------

    fn memory_doc(
        id: &str,
        salience: Option<f32>,
        status: Option<&str>,
        supersedes: Option<&str>,
    ) -> DocSummary {
        DocSummary {
            id: id.into(),
            title: format!("Memory {id}"),
            path: format!("/root/{id}.html"),
            kb_category: Some("memory-user".into()),
            kb_salience: salience,
            kb_decay: Some("fast".into()),
            kb_status: status.map(str::to_string),
            kb_supersedes: supersedes.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn atlas_points_omit_memory_fields_when_pinned_set_absent() {
        // `pinned: None` is the non-memory-scoped-kb posture — even a doc
        // that DOES carry memory metas (a stray meta on an ordinary
        // corpus) must come back with every memory field absent.
        let docs = vec![memory_doc("m1", Some(0.7), Some("forgotten"), Some("m0"))];
        let resp = build_atlas_points_response(&docs, StdPath::new("/root"), None);
        let p = &resp.points[0];
        assert_eq!(p.salience, None);
        assert_eq!(p.decay_bucket, None);
        assert_eq!(p.pinned, None);
        assert_eq!(p.forgotten, None);
        assert_eq!(p.supersedes, None);
    }

    #[test]
    fn atlas_points_carry_memory_fields_when_memory_scoped() {
        let docs = vec![
            memory_doc("m1", Some(0.7), None, Some("m0")),
            memory_doc("m2", Some(0.2), Some("forgotten"), None),
        ];
        let mut pinned_ids = std::collections::HashSet::new();
        pinned_ids.insert("m1".to_string());
        let resp = build_atlas_points_response(&docs, StdPath::new("/root"), Some(&pinned_ids));

        let m1 = resp.points.iter().find(|p| p.id == "m1").unwrap();
        assert_eq!(m1.salience, Some(0.7));
        assert_eq!(m1.decay_bucket.as_deref(), Some("fast"));
        assert_eq!(m1.pinned, Some(true));
        assert_eq!(m1.forgotten, Some(false));
        assert_eq!(m1.supersedes, Some("m0".to_string()));

        let m2 = resp.points.iter().find(|p| p.id == "m2").unwrap();
        assert_eq!(m2.salience, Some(0.2));
        assert_eq!(m2.pinned, Some(false));
        assert_eq!(m2.forgotten, Some(true));
        assert_eq!(m2.supersedes, None);
    }

    // --- W2.3a: cosine_similarity -----------------------------------------

    #[test]
    fn cosine_similarity_identical_vectors_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_opposite_vectors_is_negative_one() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        assert!((cosine_similarity(&a, &b) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_known_non_trivial_case() {
        // dot = 1*4 + 2*5 + 3*6 = 32; |a| = sqrt(14), |b| = sqrt(77).
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let expected = 32.0 / (14.0f32.sqrt() * 77.0f32.sqrt());
        assert!((cosine_similarity(&a, &b) - expected).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_guards_zero_vector_and_length_mismatch() {
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    // --- W2.3a: drop_self_hit — self-hit dropped, limit respected ---------

    #[test]
    fn drop_self_hit_removes_the_seed_id() {
        let hits = vec!["seed".to_string(), "a".to_string(), "b".to_string()];
        let kept = drop_self_hit(hits, "seed", 8);
        assert_eq!(kept, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn drop_self_hit_caps_at_limit() {
        let hits = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let kept = drop_self_hit(hits, "seed-not-present", 2);
        assert_eq!(kept, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn drop_self_hit_handles_self_hit_absent() {
        // The self-hit isn't guaranteed to be in the over-fetched window
        // (a corpus with <=limit other docs) — must not panic or drop
        // anything else.
        let hits = vec!["a".to_string(), "b".to_string()];
        let kept = drop_self_hit(hits, "seed", 8);
        assert_eq!(kept, vec!["a".to_string(), "b".to_string()]);
    }

    // --- W2.3a: SimilarResponse wire shape — the no-embedding honesty ------

    #[test]
    fn similar_response_no_embedding_shape_carries_the_reason() {
        let resp = SimilarResponse {
            neighbors: Vec::new(),
            reason: Some("no-embedding".to_string()),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["reason"], serde_json::json!("no-embedding"));
        assert_eq!(v["neighbors"], serde_json::json!([]));
    }

    #[test]
    fn similar_response_omits_reason_when_absent() {
        let resp = SimilarResponse {
            neighbors: Vec::new(),
            reason: None,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert!(
            v.as_object().unwrap().get("reason").is_none(),
            "an empty-but-real result must not carry a `reason` key: {v:?}"
        );
    }

    #[test]
    fn similar_out_omits_absent_atlas_coords() {
        let out = SimilarOut {
            id: "a".into(),
            title: "Doc A".into(),
            source_relative: "a.html".into(),
            cosine: 0.9,
            atlas_x: None,
            atlas_y: None,
            cluster: None,
        };
        let v = serde_json::to_value(&out).unwrap();
        let obj = v.as_object().unwrap();
        assert!(!obj.contains_key("atlas_x"));
        assert!(!obj.contains_key("atlas_y"));
        assert!(!obj.contains_key("cluster"));
    }

    // --- W3 T-b: fit_frame_alignment — id-joined Procrustes fit -----------

    fn frame_point(id: &str, x: f32, y: f32, cluster: i16) -> AtlasFramePoint {
        AtlasFramePoint {
            artifact_id: id.to_string(),
            x,
            y,
            cluster,
        }
    }

    #[test]
    fn fit_frame_alignment_self_alignment_is_identity_with_zero_residual() {
        let pts = vec![
            frame_point("a", 0.1, 0.2, 0),
            frame_point("b", 0.5, 0.6, 1),
            frame_point("c", 0.9, 0.1, 0),
        ];
        let (t, residual, matched) = fit_frame_alignment(&pts, &pts);
        assert_eq!(matched, 3);
        assert!((t.cos - 1.0).abs() < 1e-5);
        assert!(t.sin.abs() < 1e-5);
        assert!((t.scale - 1.0).abs() < 1e-5);
        assert!(!t.reflect);
        assert!(residual < 1e-8, "residual = {residual}");
    }

    #[test]
    fn fit_frame_alignment_pairs_by_artifact_id_not_position() {
        // Two frames naming the SAME points but in a DIFFERENT row order —
        // `procrustes::align` pairs by index, so calling it on the raw
        // slices would fit garbage; `fit_frame_alignment` must id-join
        // first. A pure translation applied to `a`/`b`/`c`, but the target
        // frame lists them out of order relative to the source.
        let frame = vec![
            frame_point("a", 0.0, 0.0, 0),
            frame_point("b", 1.0, 0.0, 1),
            frame_point("c", 1.0, 1.0, 0),
        ];
        let align_to = vec![
            frame_point("c", 1.0 + 5.0, 1.0 - 2.0, 0),
            frame_point("a", 0.0 + 5.0, 0.0 - 2.0, 0),
            frame_point("b", 1.0 + 5.0, 0.0 - 2.0, 1),
        ];
        let (t, residual, matched) = fit_frame_alignment(&frame, &align_to);
        assert_eq!(matched, 3);
        assert!(residual < 1e-6, "residual = {residual}");
        // A correct id-join recovers the pure translation (+5, -2); an
        // index-paired (unjoined) fit would NOT land here.
        let mapped = procrustes::apply(&t, &[(0.0, 0.0)])[0];
        assert!((mapped.0 - 5.0).abs() < 1e-4 && (mapped.1 - (-2.0)).abs() < 1e-4);
    }

    #[test]
    fn fit_frame_alignment_only_scores_the_overlap() {
        // "a" and "b" are shared; "only-in-frame" and "only-in-align" are
        // not — the fit and residual must be computed over exactly the
        // 2-id overlap, not fail or silently include the disjoint ids.
        let frame = vec![
            frame_point("a", 0.0, 0.0, 0),
            frame_point("b", 1.0, 0.0, 0),
            frame_point("only-in-frame", 9.0, 9.0, 2),
        ];
        let align_to = vec![
            frame_point("a", 3.0, 3.0, 0),
            frame_point("b", 4.0, 3.0, 0),
            frame_point("only-in-align", -1.0, -1.0, 3),
        ];
        let (_t, residual, matched) = fit_frame_alignment(&frame, &align_to);
        assert_eq!(matched, 2);
        assert!(residual < 1e-6, "residual = {residual}");
    }

    #[test]
    fn fit_frame_alignment_no_overlap_is_identity() {
        let frame = vec![frame_point("a", 0.1, 0.2, 0)];
        let align_to = vec![frame_point("z", 0.9, 0.9, 0)];
        let (t, _residual, matched) = fit_frame_alignment(&frame, &align_to);
        assert_eq!(matched, 0);
        assert_eq!(t, procrustes::Transform::IDENTITY);
    }

    // --- W3 T-b: AtlasFrameOut / AtlasAlignmentOut — wire projections -----

    #[test]
    fn atlas_frame_out_from_row_carries_every_field() {
        let row = AtlasFrameRow {
            id: 7,
            created_at_unix: 1_700_000_000,
            point_count: 42,
            cluster_count: 4,
            layout: "umap".to_string(),
            coord_hash: "deadbeef".to_string(),
            provenance: "recorded".to_string(),
        };
        let out = AtlasFrameOut::from(&row);
        assert_eq!(out.id, 7);
        assert_eq!(out.created_at_unix, 1_700_000_000);
        assert_eq!(out.point_count, 42);
        assert_eq!(out.cluster_count, 4);
        assert_eq!(out.layout, "umap");
        assert_eq!(out.provenance, "recorded");
        // coord_hash deliberately doesn't ride the wire shape — internal
        // dedup key, not an SPA/CLI-facing field.
        let v = serde_json::to_value(&out).unwrap();
        assert!(!v.as_object().unwrap().contains_key("coord_hash"));
    }

    #[test]
    fn atlas_alignment_out_flattens_the_transform() {
        let t = procrustes::Transform {
            cos: 0.6,
            sin: 0.8,
            scale: 2.0,
            reflect: true,
            from_centroid: (1.0, 2.0),
            to_centroid: (3.0, 4.0),
        };
        let out = AtlasAlignmentOut::from(t);
        assert_eq!(out.cos, 0.6);
        assert_eq!(out.sin, 0.8);
        assert_eq!(out.scale, 2.0);
        assert!(out.reflect);
        assert_eq!(out.from_centroid_x, 1.0);
        assert_eq!(out.from_centroid_y, 2.0);
        assert_eq!(out.to_centroid_x, 3.0);
        assert_eq!(out.to_centroid_y, 4.0);
    }

    // --- W3 T-b: AtlasPruneResponse wire shape -----------------------------

    #[test]
    fn atlas_prune_response_carries_removed_count() {
        let resp = AtlasPruneResponse { removed: 3 };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["removed"], serde_json::json!(3));
    }

    // --- W3 T-d: the reconstruction plan ----------------------------------

    fn mtime_doc(id: &str, mtime: Option<i64>) -> DocSummary {
        DocSummary {
            id: id.into(),
            title: format!("Doc {id}"),
            path: format!("/root/{id}.html"),
            mtime_unix: mtime,
            ..Default::default()
        }
    }

    #[test]
    fn build_backfill_plan_uses_mtime_and_states_its_provenance() {
        let docs = vec![
            mtime_doc("a", Some(100)),
            mtime_doc("b", Some(400)),
            mtime_doc("c", Some(700)),
            mtime_doc("d", Some(1100)),
        ];
        let (cuts, resp) = build_backfill_plan("kb1", "run-1", &docs, 4);
        assert_eq!(cuts.len(), 4);
        assert_eq!(
            resp.cuts,
            vec![
                AtlasBackfillCut {
                    cut_unix: 350,
                    doc_count: 1
                },
                AtlasBackfillCut {
                    cut_unix: 600,
                    doc_count: 2
                },
                AtlasBackfillCut {
                    cut_unix: 850,
                    doc_count: 3
                },
                AtlasBackfillCut {
                    cut_unix: 1100,
                    doc_count: 4
                },
            ]
        );
        // The honesty fields are ON THE WIRE, not left to the client to
        // infer: every frame this run writes is `reconstructed`.
        assert_eq!(resp.provenance, "reconstructed");
        assert!(resp.note.contains("not recorded history"), "{}", resp.note);
        assert_eq!(resp.status, "started");
        assert_eq!(resp.run, "run-1");
        assert_eq!(resp.events, "/api/events?filter=run:run-1");
        assert_eq!(resp.docs_without_mtime, 0);
    }

    #[test]
    fn build_backfill_plan_excludes_docs_without_an_mtime_and_says_how_many() {
        let docs = vec![
            mtime_doc("a", Some(100)),
            mtime_doc("b", None),
            mtime_doc("c", Some(200)),
        ];
        let (_cuts, resp) = build_backfill_plan("kb1", "run-1", &docs, 2);
        // A doc with no mtime can't be placed on the time axis, so it is in
        // NO cut's count — never silently folded into the oldest.
        assert_eq!(resp.docs_without_mtime, 1);
        assert_eq!(resp.cuts.last().unwrap().doc_count, 2);
    }

    #[test]
    fn build_backfill_plan_on_a_corpus_with_no_mtimes_is_an_honest_empty_plan() {
        let docs = vec![mtime_doc("a", None)];
        let (cuts, resp) = build_backfill_plan("kb1", "run-1", &docs, 8);
        assert!(cuts.is_empty());
        assert!(resp.cuts.is_empty());
        assert_eq!(resp.docs_without_mtime, 1);
    }

    #[test]
    fn build_backfill_plan_never_exceeds_the_requested_frame_count() {
        let docs: Vec<DocSummary> = (0..50)
            .map(|i| mtime_doc(&format!("d{i}"), Some(1_700_000_000 + i * 37)))
            .collect();
        for frames in 1..=BACKFILL_MAX_FRAMES {
            let (cuts, _resp) = build_backfill_plan("kb1", "run-1", &docs, frames);
            assert!(cuts.len() <= frames as usize, "frames = {frames}");
            // The last cut always covers the whole corpus, so the final
            // reconstructed frame is comparable to a live recompute.
            assert_eq!(cuts.last().unwrap().doc_count, 50);
        }
    }
}
