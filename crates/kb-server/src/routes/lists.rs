//! `lists` — Reading Lists (RL-track, v0.18). Multiple named, ordered
//! lists per kb; entries target a whole artifact or an anchored section.
//!
//! Routes:
//!
//! ```text
//! GET    /api/lists[?include_archived=true]          → cross-kb index
//! POST   /api/kb/{kb}/lists                          → create
//! GET    /api/kb/{kb}/lists/{id}                     → detail (ordered, enriched entries)
//! PATCH  /api/kb/{kb}/lists/{id}                     → header edits (title/description/pin/archive)
//! DELETE /api/kb/{kb}/lists/{id}                     → delete (entries cascade)
//! POST   /api/kb/{kb}/lists/{id}/entries             → add entry
//! PATCH  /api/kb/{kb}/lists/{id}/entries/{eid}       → note / re-anchor / read_override / move
//! DELETE /api/kb/{kb}/lists/{id}/entries/{eid}       → remove entry
//! POST   /api/kb/{kb}/lists/{id}/prune               → drop tombstoned entries (v0.33 X3)
//! ```
//!
//! SSE: `list.created/updated/deleted` carry `{kb, id, title}` (deleted
//! drops title); `list.entry.added/updated/removed` carry
//! `{kb, list_id, entry_id, artifact_id}`. Every payload names `kb` +
//! `list_id` — the SPA bridge's targeted invalidation depends on it.
//!
//! Read state is DERIVED at response time: one
//! `(DocSummary?, ReadingSummary)` per distinct artifact per request,
//! then `kb_core::lists::derive_read_state` per entry. No read-state is
//! stored beyond the manual `read_override`.

use crate::middleware::error_to_problem_json;
use crate::state::{KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::lists::{
    anchor_to_json, derive_read_state, est_minutes, new_entry_id, new_list_id, prune_list,
    ListEntry, ListSummary, NewListEntry, Patch, PositionSpec, ReadOverride, ReadState,
};
use kb_core::review::Anchor;
use kb_core::storage::lance::DocSummary;
use kb_core::storage::sqlite::{ListEntryRow, ListRow};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug, Default, Deserialize)]
pub struct IndexQuery {
    /// Archived lists are hidden from the index by default; the SPA's
    /// collapsed "Archived" section opts in.
    #[serde(default)]
    pub include_archived: bool,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ListIndexResponse")
)]
#[derive(Debug, Serialize)]
pub struct IndexResponse {
    pub lists: Vec<ListSummary>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ListDetailResponse")
)]
#[derive(Debug, Serialize)]
pub struct DetailResponse {
    pub list: ListSummary,
    pub entries: Vec<ListEntry>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ListCreateBody")
)]
#[derive(Debug, Deserialize)]
pub struct CreateBody {
    pub title: String,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub description: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub pinned: bool,
}

/// PATCH body for the list header. `description` is tri-state on the
/// wire (absent = keep, `null` = clear, string = set) via the
/// double-option trick; the TS shape is hand-written in the SPA's api
/// client (ts-rs can't express nested Option).
#[derive(Debug, Default, Deserialize)]
pub struct ListPatchBody {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ListEntryCreateBody")
)]
#[derive(Debug, Deserialize)]
pub struct EntryCreateBody {
    /// Exact 12-hex artifact id. Either this or `path` is required.
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub artifact_id: Option<String>,
    /// Source-relative path — resolved via the path-hash id derivation.
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub path: Option<String>,
    /// Target within the artifact; absent = the whole artifact.
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub anchor: Option<Anchor>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub note: Option<String>,
    /// Placement (precedence: before > after > position; default last).
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub before: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub after: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "ts-export", ts(optional = nullable))]
    pub position: Option<u32>,
}

/// PATCH body for one entry. `note` and `anchor` are tri-state (absent /
/// null / value); `read_override` uses explicit strings
/// (`"read" | "unread" | "clear"`) instead. TS shape hand-written in the
/// SPA api client.
#[derive(Debug, Default, Deserialize)]
pub struct EntryPatchBody {
    #[serde(default, deserialize_with = "double_option")]
    pub note: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub anchor: Option<Option<Anchor>>,
    pub read_override: Option<String>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub position: Option<u32>,
}

/// Serde helper: a field that distinguishes absent (outer `None`) from
/// JSON `null` (inner `None`). Pair with `#[serde(default)]`.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: serde::Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    serde::Deserialize::deserialize(de).map(Some)
}

/// One artifact's response-time enrichment: lance projection + the
/// cross-visit reading summary. Built once per distinct artifact per
/// request, shared by every entry that targets it.
struct Enrichment {
    doc: Option<DocSummary>,
    reading: kb_core::reading::ReadingSummary,
}

impl Enrichment {
    fn empty() -> Self {
        Enrichment {
            doc: None,
            reading: kb_core::reading::summarize(&[], &[], false),
        }
    }
}

/// Build the per-artifact enrichment map for a set of entry rows.
///
/// Two batched storage-actor round-trips for the WHOLE deduped id set
/// (was two per distinct artifact, serially): one lance `id IN (…)` scan
/// (`get_by_ids`) + one grouped sqlite reading query
/// (`reading_inputs_for_artifacts`). Read-state stays DERIVED per entry
/// (invariant #25) — only the fetch pattern batches. Every distinct
/// artifact id in `rows` is present in the returned map (doc `None` =
/// tombstone, empty reading summary when the artifact was never opened).
async fn enrich_artifacts(
    ctx: &KbContext,
    rows: &[ListEntryRow],
    user: &str,
) -> HashMap<String, Enrichment> {
    // Dedup the artifact id set — an artifact can appear in many entries
    // (and, for the index roll-up, across many lists).
    let mut ids: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for row in rows {
        if seen.insert(row.artifact_id.as_str()) {
            ids.push(row.artifact_id.clone());
        }
    }
    if ids.is_empty() {
        return HashMap::new();
    }

    let mut doc_by_id: HashMap<String, DocSummary> = HashMap::new();
    for d in ctx
        .storage
        .get_by_ids(ids.clone())
        .await
        .unwrap_or_default()
    {
        doc_by_id.insert(d.id.clone(), d);
    }
    // v0.34 Y1 — requester's reading progress only.
    let reading = match ctx
        .storage
        .reading_inputs_for_artifacts(ids.clone(), Some(user.to_string()))
        .await
    {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e,
                "reading_inputs_for_artifacts failed; deriving from empty");
            HashMap::new()
        }
    };

    let mut map: HashMap<String, Enrichment> = HashMap::with_capacity(ids.len());
    for id in ids {
        let doc = doc_by_id.remove(&id);
        let summary = match reading.get(&id) {
            Some((sections, visits)) => kb_core::reading::summarize(sections, visits, false),
            None => kb_core::reading::summarize(&[], &[], false),
        };
        map.insert(
            id,
            Enrichment {
                doc,
                reading: summary,
            },
        );
    }
    map
}

/// Project one stored row into the wire shape: decode the anchor, derive
/// the read state, pick the word source (per-section estimate for
/// anchored entries, lance `word_count` otherwise), join the lance
/// projection (tombstone when the artifact left lance).
///
/// `user_override` is the requester's per-user value from
/// `list_entry_user_state` (v0.34 X1). The legacy `list_entries.read_override`
/// column is frozen and never read here.
fn assemble_entry(
    kb: &str,
    source_path: &std::path::Path,
    row: ListEntryRow,
    enrich: &Enrichment,
    user_override: Option<&str>,
) -> ListEntry {
    let anchor: Option<Anchor> =
        row.anchor_json
            .as_deref()
            .and_then(|j| match serde_json::from_str::<Anchor>(j) {
                Ok(a) => Some(a),
                Err(e) => {
                    tracing::warn!(entry_id = %row.id, error = %e,
                    "corrupt list-entry anchor JSON; rendering as whole-artifact");
                    None
                }
            });
    let read_override = user_override.and_then(|s| ReadOverride::from_str(s).ok());
    // P8 / invariant #25 — SUPPRESS read-state derivation for session entries:
    // a captured transcript has no meaningful "read" progress, so a
    // read/unread badge would mislead. The UI hides the badge when is_session.
    let is_session =
        enrich.doc.as_ref().and_then(|d| d.kb_category.as_deref()) == Some("memory-session");
    let read_state = if is_session {
        ReadState::Unread
    } else {
        derive_read_state(anchor.as_ref(), &enrich.reading, read_override)
    };
    let words: Option<u32> = if anchor.is_some() {
        row.words.and_then(|w| u32::try_from(w).ok())
    } else {
        enrich.doc.as_ref().and_then(|d| d.word_count)
    };
    let est = words.map(est_minutes).filter(|m| *m > 0);
    let (title, source_relative, folder, tombstone) = match &enrich.doc {
        Some(d) => (
            Some(d.title.clone()),
            Some(kb_core::paths::doc_rel_path(&d.path, source_path)),
            Some(kb_core::paths::doc_folder(&d.path, source_path)),
            false,
        ),
        None => (None, None, None, true),
    };
    ListEntry {
        kb: kb.to_string(),
        id: row.id,
        list_id: row.list_id,
        artifact_id: row.artifact_id,
        position: u32::try_from(row.position).unwrap_or(0),
        anchor,
        note: row.note,
        read_override,
        anchor_stale: row.anchor_stale,
        words,
        est_minutes: est,
        read_state,
        is_session,
        title,
        source_relative,
        folder,
        tombstone,
        created_at: row.created_at_unix,
        updated_at: row.updated_at_unix,
    }
}

/// Roll a list's assembled entries up into the index/header shape.
fn summarize_list(kb: &str, row: ListRow, entries: &[ListEntry]) -> ListSummary {
    let mut read = 0u32;
    let mut in_progress = 0u32;
    let mut unread = 0u32;
    let mut total_minutes = 0u32;
    let mut remaining_minutes = 0u32;
    for e in entries {
        let m = e.est_minutes.unwrap_or(0);
        total_minutes = total_minutes.saturating_add(m);
        match e.read_state {
            ReadState::Read => read += 1,
            ReadState::InProgress => {
                in_progress += 1;
                remaining_minutes = remaining_minutes.saturating_add(m);
            }
            ReadState::Unread => {
                unread += 1;
                remaining_minutes = remaining_minutes.saturating_add(m);
            }
        }
    }
    ListSummary {
        kb: kb.to_string(),
        id: row.id,
        title: row.title,
        description: row.description,
        pinned: row.pinned,
        archived: row.archived,
        created_at: row.created_at_unix,
        updated_at: row.updated_at_unix,
        entry_count: entries.len() as u32,
        read_count: read,
        in_progress_count: in_progress,
        unread_count: unread,
        total_minutes,
        remaining_minutes,
    }
}

/// Assemble one list's ordered entries + summary from an already-built
/// enrichment map. Split out of [`load_list`] so the index roll-up can
/// enrich ONCE across all of a kb's lists (deduped) and reuse the map per
/// list, instead of re-fetching per list.
///
/// `user_overrides` is keyed by entry id (from `list_entry_user_state`
/// for the requester — X-phase = operator).
fn assemble_list(
    kb: &str,
    source_path: &std::path::Path,
    row: ListRow,
    entry_rows: Vec<ListEntryRow>,
    enrich: &HashMap<String, Enrichment>,
    user_overrides: &HashMap<String, String>,
) -> (ListSummary, Vec<ListEntry>) {
    let entries: Vec<ListEntry> = entry_rows
        .into_iter()
        .map(|r| {
            let ov = user_overrides.get(&r.id).map(String::as_str);
            let e = enrich
                .get(&r.artifact_id)
                .map(|e| assemble_entry(kb, source_path, r.clone(), e, ov));
            e.unwrap_or_else(|| assemble_entry(kb, source_path, r, &Enrichment::empty(), ov))
        })
        .collect();
    let summary = summarize_list(kb, row, &entries);
    (summary, entries)
}

/// Fetch + assemble one list's entries (display order) and its summary.
async fn load_list(
    ctx: &KbContext,
    kb: &str,
    row: ListRow,
    user: &str,
) -> kb_core::Result<(ListSummary, Vec<ListEntry>)> {
    let list_id = row.id.clone();
    let rows = ctx.storage.list_entries_for_list(list_id.clone()).await?;
    let enrich = enrich_artifacts(ctx, &rows, user).await;
    let user_overrides = ctx
        .storage
        .list_entry_user_overrides_for_list(list_id, user.to_string())
        .await
        .unwrap_or_default();
    Ok(assemble_list(
        kb,
        &ctx.source_path,
        row,
        rows,
        &enrich,
        &user_overrides,
    ))
}

/// `GET /api/lists` — cross-kb index, pinned-first per kb, archived
/// hidden unless `?include_archived=true`. One kb's failure is logged
/// and skipped (must not poison the fleet view).
pub async fn index(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Query(q): Query<IndexQuery>,
) -> Response<Body> {
    // FF-D — fan out each kb's list roll-ups concurrently (bounded), flattening
    // in BTreeMap order. Pure reads. Per-kb and per-list failures stay logged +
    // skipped (one kb must not poison the fleet view).
    let q = &q;
    let user = identity.user.clone();
    let mut futs: Vec<super::CorpusFut<'_, Vec<ListSummary>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        let user = user.clone();
        futs.push(Box::pin(async move {
            let lists = match ctx.storage.lists_all().await {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "lists_all failed");
                    return Vec::new();
                }
            };
            // Fetch every visible list's entry rows, then enrich ONCE over the
            // union of artifact ids (deduped across all lists) — two batched
            // storage calls per kb instead of two per distinct artifact per
            // list. Preserves the `lists_all` order; a per-list entries-fetch
            // failure is logged + the list skipped (parity with the old
            // per-list `load_list` error path).
            let mut per_list: Vec<(ListRow, Vec<ListEntryRow>)> = Vec::new();
            let mut all_rows: Vec<ListEntryRow> = Vec::new();
            for row in lists {
                if row.archived && !q.include_archived {
                    continue;
                }
                match ctx.storage.list_entries_for_list(row.id.clone()).await {
                    Ok(rows) => {
                        all_rows.extend(rows.iter().cloned());
                        per_list.push((row, rows));
                    }
                    Err(e) => {
                        tracing::warn!(kb = %kb_name, error = %e, "list roll-up failed");
                    }
                }
            }
            let enrich = enrich_artifacts(ctx, &all_rows, &user).await;
            // Per-list user overrides (X-phase = operator). Fetch serially
            // per list — index is a roll-up of counts; a few extra reads
            // beat a cross-list join.
            let mut out = Vec::with_capacity(per_list.len());
            for (row, rows) in per_list {
                let ovs = ctx
                    .storage
                    .list_entry_user_overrides_for_list(row.id.clone(), user.clone())
                    .await
                    .unwrap_or_default();
                out.push(
                    assemble_list(kb_name.as_str(), &ctx.source_path, row, rows, &enrich, &ovs).0,
                );
            }
            out
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let out: Vec<ListSummary> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect();
    Json(IndexResponse { lists: out }).into_response()
}

/// `POST /api/kb/{kb}/lists` — create. 201 + the (empty) summary.
/// 409 on a case-insensitive title clash.
pub async fn create(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Json(body): Json<CreateBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let title = body.title.trim().to_string();
    if title.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "list title must not be empty".into(),
        ));
    }
    let now_unix = chrono::Utc::now().timestamp();
    let row = match ctx
        .storage
        .list_create(
            new_list_id(),
            title,
            body.description.clone(),
            body.pinned,
            now_unix,
        )
        .await
    {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    ctx.bus.emit(
        "list.created",
        serde_json::json!({
            "kb": kb_name.as_str(),
            "id": row.id,
            "title": row.title,
        }),
    );
    let summary = summarize_list(kb_name.as_str(), row, &[]);
    (StatusCode::CREATED, Json(summary)).into_response()
}

/// `GET /api/kb/{kb}/lists/{id}` — detail: header summary + ordered,
/// enriched entries.
pub async fn detail(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let row = match ctx.storage.list_get(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "list {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    match load_list(ctx, kb_name.as_str(), row, identity.user.as_str()).await {
        Ok((list, entries)) => Json(DetailResponse { list, entries }).into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

/// `PATCH /api/kb/{kb}/lists/{id}` — header edits. Emits `list.updated`.
pub async fn update(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id)): Path<(String, String)>,
    Json(body): Json<ListPatchBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if let Some(t) = body.title.as_deref() {
        if t.trim().is_empty() {
            return error_to_problem_json(&kb_core::Error::BadRequest(
                "list title must not be empty".into(),
            ));
        }
    }
    let description: Patch<String> = match body.description {
        None => Patch::Keep,
        Some(None) => Patch::Clear,
        Some(Some(s)) => Patch::Set(s),
    };
    let now_unix = chrono::Utc::now().timestamp();
    let row = match ctx
        .storage
        .list_update(
            id.clone(),
            body.title.map(|t| t.trim().to_string()),
            description,
            body.pinned,
            body.archived,
            now_unix,
        )
        .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "list {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    ctx.bus.emit(
        "list.updated",
        serde_json::json!({
            "kb": kb_name.as_str(),
            "id": row.id,
            "title": row.title,
        }),
    );
    match load_list(ctx, kb_name.as_str(), row, identity.user.as_str()).await {
        Ok((summary, _)) => Json(summary).into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

/// `DELETE /api/kb/{kb}/lists/{id}` — 204 always (idempotent). Emits
/// `list.deleted` only when a row was actually removed.
pub async fn delete(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let removed = match ctx.storage.list_delete(id.clone()).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    if removed {
        ctx.bus.emit(
            "list.deleted",
            serde_json::json!({
                "kb": kb_name.as_str(),
                "id": id,
            }),
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Resolve an add-entry target to a verified artifact id + its doc.
/// `artifact_id` wins over `path`; the artifact MUST exist in lance —
/// refusing unknown targets keeps deliberate tombstones out (bookmarks
/// precedent), while entries whose artifact later leaves lance still
/// render as tombstones.
async fn resolve_target(
    ctx: &KbContext,
    kb_name: &kb_core::types::KbName,
    artifact_id: Option<String>,
    path: Option<String>,
) -> Result<DocSummary, Response<Body>> {
    let id = match (artifact_id, path) {
        (Some(id), _) => id,
        (None, Some(p)) => kb_core::ids::ArtifactId::from_path(&p).as_str().to_string(),
        (None, None) => {
            return Err(error_to_problem_json(&kb_core::Error::BadRequest(
                "one of artifact_id or path is required".into(),
            )))
        }
    };
    match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(d)) => Ok(d),
        Ok(None) => Err(error_to_problem_json(&kb_core::Error::NotFound(format!(
            "artifact {id} in kb {kb_name}"
        )))),
        Err(e) => Err(error_to_problem_json(&e)),
    }
}

/// Best-effort per-section word estimate for an anchored entry: read the
/// artifact source and walk the anchor's range. `None` (unreadable file
/// or unresolvable target) is fine — the ListAnchorHook refreshes the
/// estimate on the next reindex.
async fn words_for_anchor(doc: &DocSummary, anchor: &Anchor) -> Option<i64> {
    let html = tokio::fs::read_to_string(&doc.path).await.ok()?;
    kb_core::lists::section_words(&html, anchor).map(i64::from)
}

fn position_spec(
    before: Option<String>,
    after: Option<String>,
    position: Option<u32>,
) -> PositionSpec {
    if let Some(b) = before {
        PositionSpec::Before(b)
    } else if let Some(a) = after {
        PositionSpec::After(a)
    } else if let Some(p) = position {
        PositionSpec::At(p)
    } else {
        PositionSpec::Last
    }
}

/// `POST /api/kb/{kb}/lists/{id}/entries` — add an entry. 201 + the
/// assembled entry. 404 unknown list/artifact/sibling; 409 duplicate
/// target. Emits `list.entry.added`.
pub async fn add_entry(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id)): Path<(String, String)>,
    Json(body): Json<EntryCreateBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match resolve_target(ctx, &kb_name, body.artifact_id, body.path).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };
    let (anchor_json, words) = match &body.anchor {
        Some(a) => (Some(anchor_to_json(a)), words_for_anchor(&doc, a).await),
        None => (None, None),
    };
    let now_unix = chrono::Utc::now().timestamp();
    let new = NewListEntry {
        id: new_entry_id(),
        list_id: id.clone(),
        kb: kb_name.as_str().to_string(),
        artifact_id: doc.id.clone(),
        anchor_json,
        note: body.note.clone(),
        words,
        read_override: None,
    };
    let pos = position_spec(body.before, body.after, body.position);
    let row = match ctx
        .storage
        .list_entry_add(new, pos, identity.user.clone(), now_unix)
        .await
    {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    ctx.bus.emit(
        "list.entry.added",
        serde_json::json!({
            "kb": kb_name.as_str(),
            "list_id": id,
            "entry_id": row.id,
            "artifact_id": row.artifact_id,
            "user": identity.user,
        }),
    );
    let enrich = enrich_artifacts(ctx, std::slice::from_ref(&row), identity.user.as_str()).await;
    // Fresh add never carries a read marker via this route (body has none).
    let e = enrich
        .get(&row.artifact_id)
        .map(|en| assemble_entry(kb_name.as_str(), &ctx.source_path, row.clone(), en, None))
        .unwrap_or_else(|| {
            assemble_entry(
                kb_name.as_str(),
                &ctx.source_path,
                row,
                &Enrichment::empty(),
                None,
            )
        });
    (StatusCode::CREATED, Json(e)).into_response()
}

/// `PATCH /api/kb/{kb}/lists/{id}/entries/{eid}` — content edits
/// (note / anchor / read_override) and/or a move. One
/// `list.entry.updated` per call. Re-anchoring recomputes the
/// per-section word estimate and clears the stale flag.
pub async fn update_entry(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id, eid)): Path<(String, String, String)>,
    Json(body): Json<EntryPatchBody>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Scope the entry to the list in the URL — a valid eid under the
    // wrong list must 404, not silently edit.
    let rows = match ctx.storage.list_entries_for_list(id.clone()).await {
        Ok(r) => r,
        Err(e) => return error_to_problem_json(&e),
    };
    let Some(cur) = rows.into_iter().find(|r| r.id == eid) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!(
            "entry {eid} in list {id}"
        )));
    };

    let note: Patch<String> = match body.note {
        None => Patch::Keep,
        Some(None) => Patch::Clear,
        Some(Some(s)) => Patch::Set(s),
    };
    let read_override: Patch<String> = match body.read_override.as_deref() {
        None => Patch::Keep,
        Some("clear") => Patch::Clear,
        Some(s) => match ReadOverride::from_str(s) {
            Ok(ov) => Patch::Set(ov.as_str().to_string()),
            Err(e) => return error_to_problem_json(&e),
        },
    };
    let anchor: Patch<(String, Option<i64>)> = match body.anchor {
        None => Patch::Keep,
        Some(None) => Patch::Clear,
        Some(Some(a)) => {
            let words = match ctx.storage.get_by_id(cur.artifact_id.clone()).await {
                Ok(Some(doc)) => words_for_anchor(&doc, &a).await,
                _ => None, // tombstone — the hook refreshes on reindex
            };
            Patch::Set((anchor_to_json(&a), words))
        }
    };

    let now_unix = chrono::Utc::now().timestamp();
    let has_content_patch = !(note.is_keep() && anchor.is_keep() && read_override.is_keep());
    let has_move = body.before.is_some() || body.after.is_some() || body.position.is_some();

    let mut latest = cur.clone();
    if has_content_patch {
        match ctx
            .storage
            .list_entry_update(
                eid.clone(),
                note,
                anchor,
                read_override,
                identity.user.clone(),
                now_unix,
            )
            .await
        {
            Ok(Some(r)) => latest = r,
            Ok(None) => {
                return error_to_problem_json(&kb_core::Error::NotFound(format!(
                    "entry {eid} in list {id}"
                )))
            }
            Err(e) => return error_to_problem_json(&e),
        }
    }
    if has_move {
        let pos = position_spec(body.before, body.after, body.position);
        match ctx
            .storage
            .list_entry_move(id.clone(), eid.clone(), pos, now_unix)
            .await
        {
            Ok(Some(r)) => latest = r,
            Ok(None) => {
                return error_to_problem_json(&kb_core::Error::NotFound(format!(
                    "entry {eid} in list {id}"
                )))
            }
            Err(e) => return error_to_problem_json(&e),
        }
    }
    if has_content_patch || has_move {
        ctx.bus.emit(
            "list.entry.updated",
            serde_json::json!({
                "kb": kb_name.as_str(),
                "list_id": id,
                "entry_id": eid,
                "artifact_id": latest.artifact_id,
                "user": identity.user,
            }),
        );
    }
    // Per-user override for the response (legacy column is frozen).
    let ovs = ctx
        .storage
        .list_entry_user_overrides_for_list(id.clone(), identity.user.clone())
        .await
        .unwrap_or_default();
    let ov = ovs.get(&latest.id).map(String::as_str);
    let enrich = enrich_artifacts(ctx, std::slice::from_ref(&latest), identity.user.as_str()).await;
    let e = enrich
        .get(&latest.artifact_id)
        .map(|en| assemble_entry(kb_name.as_str(), &ctx.source_path, latest.clone(), en, ov))
        .unwrap_or_else(|| {
            assemble_entry(
                kb_name.as_str(),
                &ctx.source_path,
                latest,
                &Enrichment::empty(),
                ov,
            )
        });
    Json(e).into_response()
}

#[derive(Debug, Default, Deserialize)]
pub struct ExportQuery {
    /// `json` (default) or `md`.
    pub format: Option<String>,
}

/// `GET /api/kb/{kb}/lists/{id}/export?format=json|md` — the portable
/// kb-list/1 document. The JSON body is byte-compatible with the import
/// body (true round-trip); the Markdown form is the hand/Claude-editable
/// one. `Content-Disposition: attachment` so the SPA's export links
/// download.
pub async fn export(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id)): Path<(String, String)>,
    Query(q): Query<ExportQuery>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let row = match ctx.storage.list_get(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "list {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let (list, entries) = match load_list(ctx, kb_name.as_str(), row, identity.user.as_str()).await
    {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let doc = kb_core::lists::export_doc(&list, &entries);
    let (body, content_type, ext) = match q.format.as_deref().unwrap_or("json") {
        "json" => (
            kb_core::lists::export_json(&doc),
            "application/json",
            "json",
        ),
        "md" => (
            kb_core::lists::md::to_markdown(&doc),
            "text/markdown; charset=utf-8",
            "md",
        ),
        other => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "invalid export format: {other:?} (expected one of: json | md)"
            )))
        }
    };
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, content_type.to_string()),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{id}.{ext}\""),
            ),
        ],
        body,
    )
        .into_response()
}

#[derive(Debug, Default, Deserialize)]
pub struct ImportQuery {
    /// `md` (default) or `json`.
    pub format: Option<String>,
    /// `replace` (default — true round-trip) or `append`.
    pub mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportSkipped {
    /// What the document called the entry (path or artifact id).
    #[serde(rename = "ref")]
    pub reference: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct ImportResponse {
    pub imported: usize,
    pub skipped: Vec<ImportSkipped>,
    pub list: ListSummary,
    pub entries: Vec<ListEntry>,
}

/// `POST /api/kb/{kb}/lists/{id}/import?format=md|json&mode=replace|append`
/// — bulk-load a kb-list/1 document into an EXISTING list (the CLI owns
/// create-when-missing targeting). Entries resolve by `artifact_id`
/// first, then `from_path(path)` (ids are path hashes — exports are
/// portable across kbs sharing source-relative paths); unresolvable ones
/// are skipped and reported. One transaction, ONE `list.updated` emit —
/// no per-entry SSE storm. Round-tripped entry ids keep their
/// `created_at` on replace.
pub async fn import(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path((kb, id)): Path<(String, String)>,
    Query(q): Query<ImportQuery>,
    body: String,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let list_row = match ctx.storage.list_get(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "list {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let mode = match kb_core::lists::ImportMode::from_str(q.mode.as_deref().unwrap_or("replace")) {
        Ok(m) => m,
        Err(e) => return error_to_problem_json(&e),
    };
    let doc = match q.format.as_deref().unwrap_or("md") {
        "json" => kb_core::lists::import_from_json(&body),
        "md" => kb_core::lists::md::parse_markdown(&body),
        other => Err(kb_core::Error::BadRequest(format!(
            "invalid import format: {other:?} (expected one of: md | json)"
        ))),
    };
    let doc = match doc {
        Ok(d) => d,
        Err(e) => return error_to_problem_json(&e),
    };

    let mut new_entries: Vec<NewListEntry> = Vec::new();
    let mut skipped: Vec<ImportSkipped> = Vec::new();
    for e in doc.entries {
        let reference = e
            .path
            .clone()
            .or_else(|| e.artifact_id.clone())
            .unwrap_or_else(|| "(no target)".to_string());
        // artifact_id first, then the path-hash derivation.
        let mut resolved: Option<DocSummary> = None;
        if let Some(aid) = e.artifact_id.as_deref() {
            resolved = ctx.storage.get_by_id(aid.to_string()).await.ok().flatten();
        }
        if resolved.is_none() {
            if let Some(p) = e.path.as_deref() {
                let derived = kb_core::ids::ArtifactId::from_path(p).as_str().to_string();
                resolved = ctx.storage.get_by_id(derived).await.ok().flatten();
            }
        }
        let Some(docsum) = resolved else {
            skipped.push(ImportSkipped {
                reference,
                reason: format!("artifact not found in kb {kb_name}"),
            });
            continue;
        };
        let words = match &e.anchor {
            Some(a) => words_for_anchor(&docsum, a).await,
            None => None,
        };
        new_entries.push(NewListEntry {
            id: e
                .id
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(new_entry_id),
            list_id: id.clone(),
            kb: kb_name.as_str().to_string(),
            artifact_id: docsum.id.clone(),
            anchor_json: e.anchor.as_ref().map(anchor_to_json),
            note: e.note.clone(),
            words,
            read_override: e.read_override.map(|o| o.as_str().to_string()),
        });
    }

    let now_unix = chrono::Utc::now().timestamp();
    let imported = match ctx
        .storage
        .list_import_entries(
            id.clone(),
            mode,
            new_entries,
            identity.user.clone(),
            now_unix,
        )
        .await
    {
        Ok(n) => n,
        Err(e) => return error_to_problem_json(&e),
    };
    ctx.bus.emit(
        "list.updated",
        serde_json::json!({
            "kb": kb_name.as_str(),
            "id": id,
            "title": list_row.title,
        }),
    );
    // Re-read for the response (the import renumbered + bumped the list).
    let row = match ctx.storage.list_get(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "list {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    match load_list(ctx, kb_name.as_str(), row, identity.user.as_str()).await {
        Ok((list, entries)) => Json(ImportResponse {
            imported,
            skipped,
            list,
            entries,
        })
        .into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

/// `DELETE /api/kb/{kb}/lists/{id}/entries/{eid}` — 204 always
/// (idempotent). Emits `list.entry.removed` only on an actual delete.
pub async fn remove_entry(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id, eid)): Path<(String, String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let now_unix = chrono::Utc::now().timestamp();
    let removed = match ctx
        .storage
        .list_entry_remove(id.clone(), eid.clone(), now_unix)
        .await
    {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    if let Some(row) = removed {
        ctx.bus.emit(
            "list.entry.removed",
            serde_json::json!({
                "kb": kb_name.as_str(),
                "list_id": id,
                "entry_id": eid,
                "artifact_id": row.artifact_id,
            }),
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

/// v0.33 X3 — `POST /api/kb/{kb}/lists/{id}/prune` → 200 `{"removed": N}`.
/// 404 problem+json on unknown list. Removes every tombstoned entry in
/// ONE tx; emits `list.entry.removed` per removed row (mirrors DELETE
/// entry) plus one `list.updated` when removed > 0. Never bumps the
/// index generation (invariant #25).
pub async fn prune(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let now_unix = chrono::Utc::now().timestamp();
    let (removed_n, removed_rows) = match prune_list(&ctx.storage, &id, now_unix).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    for row in &removed_rows {
        ctx.bus.emit(
            "list.entry.removed",
            serde_json::json!({
                "kb": kb_name.as_str(),
                "list_id": id,
                "entry_id": row.id,
                "artifact_id": row.artifact_id,
            }),
        );
    }
    if removed_n > 0 {
        // Title for list.updated: best-effort fetch (list still exists).
        let title = match ctx.storage.list_get(id.clone()).await {
            Ok(Some(r)) => r.title,
            _ => String::new(),
        };
        ctx.bus.emit(
            "list.updated",
            serde_json::json!({
                "kb": kb_name.as_str(),
                "id": id,
                "title": title,
            }),
        );
    }
    Json(serde_json::json!({ "removed": removed_n })).into_response()
}
