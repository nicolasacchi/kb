//! `GET /api/kb/{kb}/docs/{id}` — single artifact metadata (topic 11 §B.2).
//! v0.0.1 returns the BM25 query result for the matching id; v0.1+ may
//! expose richer fields (graph neighbors, comments count).
//!
//! v0.1 added `GET /api/kb/{kb}/docs?limit=N` — flat list of artifact
//! summaries, the SPA gallery's "show me everything" view.
//!
//! S2 (S-milestone) extends the list endpoint with server-side
//! filter/sort/pagination. New query params:
//!
//!   folder=<path>            descendant-inclusive (default)
//!   folder_exact=1|true      with folder= only: exact equality, no descendants
//!   tags=<csv>               any-of
//!   caps=<csv>               all-of  (svg|interactive|code|longread)
//!   since=<7d|30d|all>       indexed/mtime window
//!   index=1                  index-pages only
//!   sort=<recent|indexed|created|title|words|residue>
//!   dir=<asc|desc>
//!   offset=<n>               page cursor — *and* the envelope opt-in
//!   limit=<n>                page size (default 200, max 500 in
//!                            envelope mode, 50000 in legacy mode)
//!   projection=<slim|default|atlas>
//!   include=atlas            legacy alias for projection=atlas
//!
//! Response shape depends on whether the client opted into envelope
//! mode: passing any `offset=` (including `offset=0`) returns the
//! envelope `{docs, total, offset, limit, has_more}` and a `Link:
//! ...; rel="next"` header when there's more. Without `offset` the
//! route returns the bare `Vec<DocResponse>` array shape so v1
//! consumers (the SPA at HEAD, CLI scripts, the e2e test suite) keep
//! working unchanged. The legacy shape is scheduled for removal in
//! S8.

use crate::middleware::error_to_problem_json;
use crate::state::{GalleryCache, KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{ConnectInfo, Extension, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::docs_query::{
    cmp_rows, matches, paginate, Capability, DocRow, DocsQuery, GroupKey, Projection, SortDir,
    SortKey,
};
use kb_core::paths::{doc_folder, doc_rel_path};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;

const DEFAULT_PAGE_LIMIT: u32 = 200;
const MAX_ENVELOPE_LIMIT: u32 = 500;
const MAX_LEGACY_LIMIT: u32 = 50_000;
/// W2.3a — cap on the `?ids=` gallery filter (invariant #35). A lasso of
/// hundreds of docs is plausible; thousands hand-typed into a URL is not —
/// over-cap 400s (`parse_ids_filter`) rather than silently truncating.
const MAX_IDS_FILTER: usize = 500;

// ts(optional_fields): every Option field below carries
// skip_serializing_if (absent-or-value on the wire, never null), which
// maps to `field?: T`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export, optional_fields))]
#[derive(Debug, Serialize)]
pub struct DocResponse {
    pub id: String,
    pub title: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb_category: Option<String>,
    /// v0.3 — populated when `projection=atlas` (or the legacy
    /// `include=atlas`). Single-doc GET always omits these (use the
    /// list endpoint for atlas data; the SPA fetches both shapes
    /// anyway).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas_x: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas_y: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas_cluster: Option<i16>,
    /// v0.6 B1 — fields the parser already records but were not
    /// surfaced in the v0.0.1 docs endpoint. The SPA gallery card
    /// reads age + isNew from the time fields, summary from the
    /// body excerpt, and the capability glyph strip from the
    /// indicator booleans + svg_count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime_unix: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_at_unix: Option<i64>,
    /// v0.15 — filesystem birth time of the source file. Powers the
    /// gallery's "created" sort. None on btime-less filesystems and on
    /// rows indexed before v0.15.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_unix: Option<i64>,
    /// v0.33 X2 — unix seconds when kb first indexed this artifact
    /// (sqlite `doc_first_seen`, INSERT OR IGNORE — stable across reindex
    /// and mtime drift). None when the seed/lookup misses (e.g. slim
    /// projection never joins, or a race before the seed lands).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_indexed_unix: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub svg_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_canvas: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_form: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_animation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_details: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_math: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_drag: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub js_loc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub css_loc: Option<String>,
    /// v0.6 I1 — new counters. None on rows indexed before v0.6;
    /// populated by the next `kb reindex` pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_block_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longread: Option<bool>,
    /// v0.6 B2 — graph degree counts derived from the sqlite edges
    /// table. Always present on `list` responses; omitted on the
    /// single-doc `get` response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlinks: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outlinks: Option<u32>,
    /// W1.A — server-derived read-state, joined in from the per-kb
    /// reading rollup AFTER pagination — like `backlinks`/`outlinks`
    /// above, a per-request decoration over the *visible page only*,
    /// never baked into the `GalleryCache` memo (invariant #15: history
    /// writes don't bump the index generation, so a cached read-state
    /// would go stale forever). Reuses the same wire strings as the
    /// search `Hit` (`unread|in_progress|read`,
    /// `routes::search::apply_read`) so the SPA's `ReadingChip` reads
    /// both shapes identically. `slim`/`atlas` projections stay bare
    /// (`None`), matching `backlinks`/`outlinks`' own nulling above.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_state: Option<String>,
    /// Scroll completion 0..100 for the reading chip. `None` alongside a
    /// bare `read_state` (slim/atlas), or when the artifact has never
    /// been opened (an id absent from the rollup carries no percentage).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_pct: Option<u8>,
    /// Most-recent open visit's `started_at` (unix seconds). Backs the
    /// gallery card's "opened …" chip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_opened_unix: Option<i64>,
    /// CT-F4 — **session residue**: how many OTHER memories were born in
    /// this artifact's own birth session (`kb-session`), summed across the
    /// daemon's memory-scoped corpora. "Docs whose conclusions someone
    /// kept". Decorated on the visible PAGE ONLY, like `read_state` above
    /// (never baked into the `GalleryCache` memo, #15) and only on the
    /// `default` projection; **absent when zero** and absent for a doc
    /// with no `kb-session` at all — the two are deliberately
    /// indistinguishable on the wire (both mean "no residue to show"),
    /// while the `?sort=residue` comparator DOES distinguish them (a
    /// session-less doc sorts last under either `dir`).
    ///
    /// SURFACED, NEVER SCORED (#10's CT posture): this is a display
    /// decoration computed post-filter over the returned page — it is not
    /// a `DocRow`/`DocSummary` field, never reaches `docs_query::matches`,
    /// and is structurally unreachable from search ranking or
    /// `kb_core::memory`'s scoring types.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_residue: Option<u32>,
    /// v0.6 T1 — tag slugs (from `<meta name="kb-tags">` or path).
    /// Always sent (possibly empty) on list responses so the SPA can
    /// distinguish "this row has no tags" from "we haven't loaded
    /// the tag field for this row yet".
    pub tags: Vec<String>,
    /// v0.7 S1 — `<meta name="kb-status">` content. None when the
    /// artifact didn't declare one OR the row was indexed pre-v0.7.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb_status: Option<String>,
    /// v0.7 S1 — `<meta name="kb-severity">` content. Same nullability
    /// as `kb_status`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb_severity: Option<String>,
    /// v0.8 G1 — folder (parent directory) of this doc relative to the
    /// kb source root. Empty string for docs at the root. Always sent
    /// on list responses so the SPA gallery's folder filter + grouped
    /// view can index against it without re-parsing `path`.
    pub folder: String,
    /// Track U — full source-root-relative path, forward-slash
    /// separated (e.g. `ideas/foo/bar.html`). The SPA builds the
    /// path-based permalink `/a/<kb>/<source_relative>` from this; the
    /// artifact id stays internal (iframe origin only). Derived via
    /// `doc_rel_path`, same source as `folder`.
    pub source_relative: String,
}

pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // v0.1: exact-id lookup via lance SQL filter. Replaces the v0.0.1
    // BM25-and-post-filter trick which silently returned 404 for hex-id
    // lookups (those don't tokenise as text in the FTS index).
    match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(row)) => {
            let resp = single_doc_response_with_first_seen(ctx, row).await;
            Json(resp).into_response()
        }
        // F3b — moves-table fallback: old id → 301 to the same route family
        // under the new path-derived id (newest-wins chain).
        Ok(None) => moves_redirect_or_404(ctx, &kb, &id, "").await,
        Err(e) => error_to_problem_json(&e),
    }
}

/// F3b — old id → 301 to the same route family under the new path-derived id
/// (newest-wins chain). `suffix` is appended after the id so a doc SUB-route
/// keeps the chain: `""` for `GET …/docs/{id}` (`get`, above), `"/code-refs"`
/// for the DCB `coderef/1` route (`routes::coderefs::for_doc`). Extracted
/// from `get` (v0.33) when DCB W1.B added the first sibling sub-route.
pub(crate) async fn moves_redirect_or_404(
    ctx: &KbContext,
    kb: &str,
    id: &str,
    suffix: &str,
) -> Response<Body> {
    match kb_core::relocate::moves_lookup(&ctx.storage, id).await {
        Ok(Some((new_id, _new_rel))) if new_id != id => {
            let location = format!("/api/kb/{kb}/docs/{new_id}{suffix}");
            axum::http::Response::builder()
                .status(StatusCode::MOVED_PERMANENTLY)
                .header(axum::http::header::LOCATION, location)
                .body(Body::empty())
                .unwrap_or_else(|_| {
                    error_to_problem_json(&kb_core::Error::NotFound(format!("doc {id}")))
                })
        }
        Ok(_) => error_to_problem_json(&kb_core::Error::NotFound(format!("doc {id}"))),
        Err(e) => error_to_problem_json(&e),
    }
}

/// Build the single-doc `DocResponse` shape (atlas + edge counts omitted —
/// callers wanting those use the list endpoint). Shared by `get` (by id)
/// and `get_by_path` (by source-relative path). Joins `doc_first_seen` for
/// the stable first-indexed timestamp (v0.33 X2).
async fn single_doc_response_with_first_seen(
    ctx: &KbContext,
    row: kb_core::storage::lance::DocSummary,
) -> DocResponse {
    let first = ctx
        .storage
        .first_seen_for_ids(vec![row.id.clone()])
        .await
        .unwrap_or_default()
        .get(&row.id)
        .copied();
    let mut resp = single_doc_response(row, &ctx.source_path);
    resp.first_indexed_unix = first;
    resp
}

fn single_doc_response(
    row: kb_core::storage::lance::DocSummary,
    source_root: &std::path::Path,
) -> DocResponse {
    DocResponse {
        id: row.id,
        title: row.title,
        folder: doc_folder(&row.path, source_root),
        source_relative: doc_rel_path(&row.path, source_root),
        path: row.path,
        kb_category: row.kb_category,
        atlas_x: None,
        atlas_y: None,
        atlas_cluster: None,
        mtime_unix: row.mtime_unix,
        indexed_at_unix: row.indexed_at_unix,
        created_unix: row.created_unix,
        first_indexed_unix: None,
        summary: row.summary,
        svg_count: row.svg_count,
        has_canvas: row.has_canvas,
        has_form: row.has_form,
        has_animation: row.has_animation,
        has_details: row.has_details,
        has_math: row.has_math,
        has_drag: row.has_drag,
        js_loc: row.js_loc,
        css_loc: row.css_loc,
        table_count: row.table_count,
        code_block_count: row.code_block_count,
        word_count: row.word_count,
        longread: row.longread,
        backlinks: None,
        outlinks: None,
        // Single-doc GET never decorates read-state (same nullability as
        // backlinks/outlinks above) — callers wanting it use the list
        // endpoint, which joins the per-kb reading rollup for the page.
        read_state: None,
        read_pct: None,
        last_opened_unix: None,
        // CT-F4 — same nullability rule as read-state above: the single-doc
        // GET never runs the cross-corpus residue count (the list endpoint
        // does, page-scoped).
        session_residue: None,
        tags: row.tags,
        kb_status: row.kb_status,
        kb_severity: row.kb_severity,
    }
}

/// `GET /api/kb/{kb}/docs/by-path/{*path}` — resolve an artifact by its
/// source-root-relative path (forward-slash separated). Backs the SPA's
/// path-based permalink: the route splat `/a/<kb>/<rel>` is resolved here
/// to the full doc (including its `id`, which the SPA needs for the iframe
/// origin). Mirrors the path stage of `routes::lookup`; 404 on no match.
pub async fn get_by_path(
    State(state): State<Arc<KbHandles>>,
    Path((kb, rel_path)): Path<(String, String)>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    // The indexer stored absolute paths; compose against the source root
    // and look up. Try the raw join first, then the canonicalised form
    // (resolves a kb mounted under a symlink) — same ladder as lookup
    // stage 2.
    let q_norm = rel_path.trim_start_matches('/').replace('\\', "/");
    let absolute = ctx.source_path.join(&q_norm);
    let abs_str = absolute.to_string_lossy().to_string();
    match ctx.storage.get_by_source_path(abs_str.clone()).await {
        Ok(Some(row)) => {
            return Json(single_doc_response_with_first_seen(ctx, row).await).into_response();
        }
        Ok(None) => { /* fall through to canonicalised retry */ }
        Err(e) => return error_to_problem_json(&e),
    }
    if let Ok(canon) = absolute.canonicalize() {
        let canon_str = canon.to_string_lossy().to_string();
        if canon_str != abs_str {
            match ctx.storage.get_by_source_path(canon_str).await {
                Ok(Some(row)) => {
                    return Json(single_doc_response_with_first_seen(ctx, row).await)
                        .into_response();
                }
                Ok(None) => {}
                Err(e) => return error_to_problem_json(&e),
            }
        }
    }
    error_to_problem_json(&kb_core::Error::NotFound(format!("doc at path {q_norm}")))
}

/// CT-F4 — the docs-list route's sort vocabulary: every pure, row-local
/// [`SortKey`] **plus** `residue`, which has no row-local comparator at all
/// (its signal is a cross-corpus count, not a column on `DocRow`). Same
/// reasoning as W1.A's `read` facet: `kb_core::docs_query` stays untouched
/// rather than growing a `SortKey` variant whose `cmp_rows` arm couldn't be
/// implemented — the route owns the extra token, the route owns the
/// comparator (`cmp_residue`).
///
/// Deserialization is delegated to `SortKey` for every non-`residue` token,
/// so an unknown value still fails exactly the way it did before this enum
/// existed (serde "unknown variant" → 400), and `sort=recent|indexed|
/// created|title|words` stay byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListSort {
    Core(SortKey),
    Residue,
}

impl Default for ListSort {
    fn default() -> Self {
        ListSort::Core(SortKey::default())
    }
}

impl ListSort {
    /// The `SortKey` handed to `cmp_rows`. `Residue` degrades to the
    /// DEFAULT key — which is exactly what `cmp_residue` uses as its own
    /// tie-break, so there is one definition of "residue's secondary
    /// order", not two.
    fn core(self) -> SortKey {
        match self {
            ListSort::Core(k) => k,
            ListSort::Residue => SortKey::default(),
        }
    }

    fn default_dir(self) -> SortDir {
        match self {
            ListSort::Core(k) => k.default_dir(),
            // "Most kept first" is the point of the sort.
            ListSort::Residue => SortDir::Desc,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ListSort::Core(k) => sort_to_str(k),
            ListSort::Residue => "residue",
        }
    }
}

impl<'de> Deserialize<'de> for ListSort {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::IntoDeserializer;
        let raw = String::deserialize(de)?;
        if raw == "residue" {
            return Ok(ListSort::Residue);
        }
        let inner: serde::de::value::StrDeserializer<D::Error> = raw.as_str().into_deserializer();
        SortKey::deserialize(inner).map(ListSort::Core)
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct ListParams {
    pub folder: Option<String>,
    /// v0.33 X1 — when set truthy (`1`/`true`), `folder` matches by
    /// equality only (no descendants). Absent/`0`/`false` keeps the
    /// historical descendant-inclusive behaviour. Only meaningful when
    /// `folder` is also set.
    #[serde(default, deserialize_with = "deserialize_boolish_opt")]
    pub folder_exact: Option<bool>,
    /// Comma-separated tag slugs; any-of match.
    pub tags: Option<String>,
    /// Comma-separated capabilities; all-of match.
    pub caps: Option<String>,
    /// `7d`, `30d`, or `all`. Anything else (including absent) means
    /// "no time filter".
    pub since: Option<String>,
    /// v0.22 — positive `kb-category` include (exact match). Powers the
    /// reader's clickable-category → gallery pivot. Distinct from the
    /// route-injected note exclusion below.
    pub category: Option<String>,
    /// v0.22 — absolute `mtime_unix` window bounds (unix seconds, inclusive).
    /// `from`/`to` power the reader's "files modified around this time" pivot;
    /// either may be set alone (open-ended).
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// W2.3a — csv id-set filter (invariant #35's atlas-lasso / working-set
    /// gallery pivot — `galleryUrl(kb, {ids})`). Route-injected into
    /// `DocsQuery.ids`, gated in `matches` across every OR branch (a
    /// `docs_query` DSL atom would be overkill for a set that only ever
    /// comes from the SPA's own id list, never hand-typed). Sanitised to the
    /// id charset + capped at `MAX_IDS_FILTER` by `parse_ids_filter`; an
    /// over-cap request 400s rather than silently truncating a lasso.
    pub ids: Option<String>,
    /// W1.A — csv read-state facet: `never-opened|unread|in_progress|read`.
    /// A route-side set-membership filter over the per-kb reading rollup
    /// (NOT a `docs_query` DSL atom — `kb_core::docs_query` stays
    /// untouched); applied AFTER the `docs_query` filter pass, before
    /// sort + paginate. Mirrors `routes::search::parse_read`, extended
    /// with `never-opened` (an id entirely absent from the rollup map —
    /// distinct from a rollup entry whose *state* happens to be Unread
    /// via a list read-override).
    pub read: Option<String>,
    /// `1` to include only index/landing pages.
    pub index: Option<u8>,
    /// CT-F4 — `recent|indexed|created|title|words` (the `docs_query`
    /// keys, unchanged) plus `residue`. See [`ListSort`].
    pub sort: Option<ListSort>,
    pub dir: Option<SortDir>,
    /// S7 — `folder` prepends folder ASC to the primary sort so the
    /// SPA's gallery can render section-grouped views over the
    /// paginated stream. Default `none`.
    pub group: Option<GroupKey>,
    /// Pagination cursor. Presence of this param (any value, including
    /// 0) is also the envelope-mode opt-in.
    pub offset: Option<u32>,
    pub limit: Option<u32>,
    pub projection: Option<Projection>,
    /// Legacy opt-in alias for `projection=atlas`. Kept for v0.3
    /// callers; new code should use `projection=`.
    pub include: Option<String>,
    /// Force envelope mode even without an `offset=`. Mostly for tests
    /// and CLI scripts; the SPA opts in via `offset=`.
    pub envelope: Option<u8>,
    /// v0.10 Q2 — structured query DSL. When present, the parsed AST's
    /// linearised DocsQuery is unioned with the flat params (tags/caps
    /// appended; folder/since/index_only taken from `q` when set, else
    /// from the flat form). Lets the SPA round-trip the QueryRibbon
    /// state without parameter explosion.
    pub q: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DocsListResponse {
    pub docs: Vec<DocResponse>,
    /// Post-filter count, *before* pagination. SPA renders this as the
    /// "12 of 288" badge.
    pub total: u32,
    pub offset: u32,
    pub limit: u32,
    pub has_more: bool,
    /// v0.10 Q2 — route timing in milliseconds (filter + sort + slice).
    /// Lit up by the QueryRibbon's "… in N ms" counter (D3 currently
    /// hardcodes 0 ms; Q3 will read this field).
    pub ms: u64,
    /// v0.10 Q2 — diagnostic warnings from the query parser (OR
    /// collapses, unknown keys, NOT-on-fields-we-can't-exclude). Empty
    /// when `q` was absent or parsed cleanly.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub query_warnings: Vec<String>,
}

pub async fn list(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path(kb): Path<String>,
    Query(params): Query<ListParams>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let user = identity.user.clone();

    // Envelope mode when the client passed `offset=` or `envelope=1`.
    // Without those, fall through to the legacy bare-array shape that
    // matches the v0.1 contract (the SPA at HEAD relies on this until
    // S5 ships).
    let envelope_mode = params.offset.is_some() || params.envelope == Some(1);

    let projection = resolve_projection(&params);
    // CT-F4 — `sort_spec` is the wire vocabulary (`SortKey` + `residue`);
    // `sort` is the row-local key `cmp_rows` understands. With `?sort=`
    // absent BOTH resolve exactly as they did pre-CT-F4
    // (`SortKey::default()` + its `default_dir()`), so the default order is
    // byte-identical.
    let sort_spec = params.sort.unwrap_or_default();
    let sort = sort_spec.core();
    let dir = params.dir.unwrap_or_else(|| sort_spec.default_dir());
    let group = params.group.unwrap_or_default();
    let offset = params.offset.unwrap_or(0);
    let max_limit = if envelope_mode {
        MAX_ENVELOPE_LIMIT
    } else {
        MAX_LEGACY_LIMIT
    };
    let limit = params.limit.unwrap_or(DEFAULT_PAGE_LIMIT).min(max_limit);
    let started = std::time::Instant::now();

    // W2.3a — sanitise + cap the id-set filter before touching storage; an
    // over-cap request 400s rather than silently truncating a lasso.
    let ids_filter = match parse_ids_filter(params.ids.as_deref()) {
        Ok(v) => v,
        Err(detail) => return error_to_problem_json(&kb_core::Error::BadRequest(detail)),
    };

    // Build the query for docs_query.
    let mut q = DocsQuery {
        folder: params.folder.clone(),
        // v0.33 X1 — exact-folder gate; default false = descendant-inclusive.
        folder_exact: params.folder_exact.unwrap_or(false),
        tags: split_csv(params.tags.as_deref()),
        caps: parse_caps(params.caps.as_deref()),
        since_unix: parse_since(params.since.as_deref()),
        // v0.22 — positive category include + absolute mtime window. Both are
        // flat-param gates (no `?q=` DSL atom) applied across every OR branch.
        category: params
            .category
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        mtime_from: params.from,
        mtime_to: params.to,
        // W2.3a — same flat-param, every-OR-branch treatment as category/mtime.
        ids: ids_filter,
        index_only: params.index == Some(1),
        exclude_categories: Vec::new(),
        // N-track — editable notes (Markdown + `kb-category: note`) have their
        // own `/notes` view + contextual panel; keep them out of the document
        // grid + atlas. Markdown-aware on purpose: HTML artifacts that merely
        // use `note` as a content category are NOT notes and stay in the grid
        // (a flat `exclude_categories=["note"]` would silently hide them).
        // Notes stay searchable (search bypasses `matches`) + resolvable by id/path.
        exclude_notes: true,
        // v0.16 Q-track — NOT-exclusions + OR fan-out, populated from the
        // `?q=` DSL overlay below (flat params set none of these).
        exclude_tags: Vec::new(),
        exclude_folders: Vec::new(),
        exclude_caps: Vec::new(),
        exclude_index: false,
        alternatives: Vec::new(),
        sort,
        dir,
        group,
        offset,
        limit,
        projection,
    };
    // v0.10 Q2 — overlay the DSL on top of the flat params. The DSL's
    // tags/caps are appended (any-of), folder/since/index_only override
    // the flat form when the DSL sets them, else the flat form stays.
    // Parse errors fall through with the flat-params filter intact so a
    // malformed `?q=` doesn't black-hole the gallery.
    let mut query_warnings: Vec<String> = Vec::new();
    if let Some(raw) = params.q.as_deref().filter(|s| !s.trim().is_empty()) {
        match kb_core::query::parse(raw) {
            Ok(expr) => {
                let conv = expr.to_docs_query();
                q.tags.extend(conv.query.tags);
                q.caps.extend(conv.query.caps);
                if conv.query.folder.is_some() {
                    q.folder = conv.query.folder;
                }
                if conv.query.since_unix.is_some() {
                    q.since_unix = conv.query.since_unix;
                }
                q.index_only = q.index_only || conv.query.index_only;
                // v0.16 Q-track — carry the NOT-exclusions + OR fan-out.
                // `exclude_notes` stays the route's gate (applied across
                // every OR branch by `matches`); the DSL never sets it.
                // Alternatives replace (don't extend): a fresh `?q=` defines
                // its own OR set.
                q.exclude_tags.extend(conv.query.exclude_tags);
                q.exclude_folders.extend(conv.query.exclude_folders);
                q.exclude_caps.extend(conv.query.exclude_caps);
                q.exclude_index = q.exclude_index || conv.query.exclude_index;
                q.alternatives = conv.query.alternatives;
                query_warnings = conv.warnings;
            }
            Err(e) => {
                query_warnings.push(format!("query parse error: {e}"));
            }
        }
    }

    // Acquire the decorated row-set + edge counts. For the gallery
    // (default/slim) projection these come from a per-(kb, index-
    // generation) memo (P1), so concurrent list requests share ONE
    // `list_docs(u32::MAX)` scan + `edge_counts()` GROUP BY instead of
    // each re-running both. The atlas projection is the AtlasView's wider
    // scan, served uncached (lower frequency; P3 owns atlas perf).
    let (rows, edge_counts) = if projection == Projection::Atlas {
        let raw = match ctx.storage.list_docs_with_atlas(u32::MAX).await {
            Ok(r) => r,
            Err(e) => return error_to_problem_json(&e),
        };
        let decorated = Arc::new(decorate_rows(raw, &ctx.source_path));
        let edges = Arc::new(ctx.storage.edge_counts().await.unwrap_or_default());
        (decorated, edges)
    } else {
        match gallery_snapshot(ctx).await {
            Ok(snap) => snap,
            Err(e) => return error_to_problem_json(&e),
        }
    };

    // Filter + sort over borrowed refs into the (possibly shared,
    // immutable) row-set, materialising only the visible page — avoids
    // an O(corpus) clone of the cached Vec per request. The lance scan
    // applies no predicate, so this one cached Vec serves every
    // filter/sort/group/page/`q` combination.
    let mut filtered: Vec<&DocRow> = rows.iter().filter(|r| matches(r, &q)).collect();

    // W1.A — `read` facet: a route-side set-membership filter over the
    // per-kb reading rollup, applied AFTER the docs_query pass and BEFORE
    // sort + paginate. `docs_query.rs` stays untouched — the signal isn't
    // on `DocSummary`/`DocRow`, so widening `matches` would mean threading
    // a rollup map through a pure function that's tested (and reused by
    // the folder/tag aggregators) without one. One extra whole-table fetch
    // per request when the facet is active; never cached (invariant #15).
    let read_tokens = parse_read(params.read.as_deref());
    if !read_tokens.is_empty() {
        let full_rollup = ctx
            .storage
            .reading_rollup(user.clone())
            .await
            .unwrap_or_default();
        filtered.retain(|r| read_token_matches(&r.doc.id, &full_rollup, &read_tokens));
    }

    // CT-F4 — `?sort=residue` orders by "how many memories were kept from
    // this doc's own birth session", which is a cross-corpus count, not a
    // column: the ONE batched read (`residue_counts`, a grouped count per
    // memory-scoped corpus, fanned out per #28) therefore runs BEFORE the
    // sort, over the whole surviving set's session ids. Every other sort
    // path leaves the map empty here and fills it AFTER pagination, over
    // the page's ids only (below) — so a request runs at most one such
    // read, never one per doc.
    let residue_self_counted = ctx.memory_scope.is_some();
    let mut residue: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    if sort_spec == ListSort::Residue {
        let sids = distinct_session_ids(filtered.iter().map(|r| &r.doc));
        residue = residue_counts(&state, sids).await;
        filtered.sort_by(|a, b| cmp_residue(a, b, group, dir, &residue, residue_self_counted));
    } else {
        filtered.sort_by(|a, b| cmp_rows(a, b, group, sort, dir));
    }
    let (page, total) = paginate(filtered, offset, limit);

    // W1.A — decorate the visible PAGE ONLY (never the whole corpus, never
    // the `GalleryCache` memo) with the per-kb reading rollup. Scoped to
    // `default` projection — `slim`/`atlas` stay bare, mirroring how they
    // already null backlinks/outlinks/summary/etc. below in
    // `build_response`. One `reading_rollup_for_ids` round-trip per
    // request, bounded by the page size (≤ `MAX_ENVELOPE_LIMIT`/
    // `MAX_LEGACY_LIMIT`), not the corpus.
    // v0.33 X2 — same page-scoped join for `first_indexed_unix` (sqlite
    // `doc_first_seen`); independent of projection so slim/atlas also get
    // a stable created-anchor when the SPA asks.
    let page_ids: Vec<String> = page.iter().map(|r| r.doc.id.clone()).collect();
    let read_rollup_page: Option<std::collections::HashMap<String, kb_core::reading::ReadRollup>> =
        if projection == Projection::Default {
            Some(
                ctx.storage
                    .reading_rollup_for_ids(page_ids.clone(), user)
                    .await
                    .unwrap_or_default(),
            )
        } else {
            None
        };
    let first_seen_page: std::collections::HashMap<String, i64> = ctx
        .storage
        .first_seen_for_ids(page_ids)
        .await
        .unwrap_or_default();

    // CT-F4 — page-scoped residue join for every non-`residue` sort (the
    // residue sort already holds a map covering this page, computed above).
    // Scoped to `default` projection like the read-state join, and skipped
    // entirely when no row on the page carries a `kb-session` — a daemon
    // with no memory-scoped corpus, or a page of un-stamped artifacts, pays
    // nothing.
    if sort_spec != ListSort::Residue && projection == Projection::Default {
        let sids = distinct_session_ids(page.iter().map(|r| &r.doc));
        residue = residue_counts(&state, sids).await;
    }

    let docs: Vec<DocResponse> = page
        .into_iter()
        .map(|row| {
            let session_residue = if projection == Projection::Default {
                session_residue_for(&row.doc, &residue, residue_self_counted)
            } else {
                None
            };
            let mut resp = build_response(
                (*row).clone(),
                &edge_counts,
                projection,
                &ctx.source_path,
                read_rollup_page.as_ref(),
            );
            resp.first_indexed_unix = first_seen_page.get(&resp.id).copied();
            resp.session_residue = session_residue;
            resp
        })
        .collect();

    if envelope_mode {
        let has_more = offset.saturating_add(limit) < total;
        let ms = started.elapsed().as_millis() as u64;
        let body = DocsListResponse {
            docs,
            total,
            offset,
            limit,
            has_more,
            ms,
            query_warnings,
        };
        let mut resp = Json(body).into_response();
        if has_more {
            let next = format!(
                "</api/kb/{kb}/docs?{}>; rel=\"next\"",
                rebuild_query(&params, offset.saturating_add(limit))
            );
            if let Ok(v) = HeaderValue::from_str(&next) {
                resp.headers_mut().insert(header::LINK, v);
            }
        }
        resp
    } else {
        Json(docs).into_response()
    }
}

/// Annotate each lance row with its folder (relative to the kb source
/// root) so `docs_query` can run its predicates without seeing the
/// source path.
fn decorate_rows(
    rows: Vec<kb_core::storage::lance::DocSummary>,
    source_root: &std::path::Path,
) -> Vec<DocRow> {
    rows.into_iter()
        .map(|doc| {
            let folder = doc_folder(&doc.path, source_root);
            DocRow { doc, folder }
        })
        .collect()
}

/// P1 — return the gallery row-set + edge counts for `ctx`, served from
/// the per-(kb, index-generation) memo when warm. On a generation change
/// (or the first call) it runs the one `list_docs(u32::MAX)` scan + folder
/// decoration + `edge_counts()` GROUP BY and publishes the snapshot, so
/// concurrent gallery requests at the same generation share one scan.
///
/// Correctness: the snapshot is published under the generation observed
/// *before* the scan. A racing mutation that bumps the generation mid-scan
/// only forces the next request to miss and rebuild — never a stale serve,
/// because a cache hit requires `stored.generation == index_generation()`
/// and the counter is monotonic. The `std::sync::Mutex` guard is always
/// dropped before any `.await`.
pub(crate) async fn gallery_snapshot(
    ctx: &KbContext,
) -> Result<
    (
        Arc<Vec<DocRow>>,
        Arc<std::collections::HashMap<String, (u32, u32)>>,
    ),
    kb_core::Error,
> {
    let generation = ctx.storage.index_generation();
    {
        let guard = ctx.gallery_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref() {
            if c.generation == generation {
                return Ok((Arc::clone(&c.rows), Arc::clone(&c.edge_counts)));
            }
        }
    } // release the std Mutex BEFORE the awaits below — never held across .await

    let raw = ctx.storage.list_docs(u32::MAX).await?;
    let rows = Arc::new(decorate_rows(raw, &ctx.source_path));
    let edge_counts = Arc::new(ctx.storage.edge_counts().await.unwrap_or_default());

    {
        let mut guard = ctx.gallery_cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(GalleryCache {
            generation,
            rows: Arc::clone(&rows),
            edge_counts: Arc::clone(&edge_counts),
        });
    }
    Ok((rows, edge_counts))
}

fn resolve_projection(p: &ListParams) -> Projection {
    if let Some(proj) = p.projection {
        return proj;
    }
    // Legacy v0.3: `include=atlas` is equivalent to projection=atlas.
    if p.include
        .as_deref()
        .map(|s| s.split(',').any(|t| t.trim() == "atlas"))
        .unwrap_or(false)
    {
        return Projection::Atlas;
    }
    Projection::Default
}

fn split_csv(s: Option<&str>) -> Vec<String> {
    match s {
        Some(s) => s
            .split(',')
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .map(|t| t.to_string())
            .collect(),
        None => Vec::new(),
    }
}

/// v0.33 X1 — query-string bool-ish for `folder_exact`. Accepts the common
/// SPA/`?flag=1` forms (`1`/`true`/`yes` → true; `0`/`false`/`no` → false;
/// absent → None). Unknown tokens degrade to None (no exact gate) rather
/// than 400, matching the rest of the docs grammar's soft-parse style.
fn deserialize_boolish_opt<'de, D>(de: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(de)?;
    Ok(match opt.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
    })
}

fn parse_caps(s: Option<&str>) -> Vec<Capability> {
    match s {
        Some(s) => s
            .split(',')
            .filter_map(|tok| match tok.trim() {
                "svg" => Some(Capability::Svg),
                "interactive" => Some(Capability::Interactive),
                "code" => Some(Capability::Code),
                "longread" => Some(Capability::Longread),
                _ => None,
            })
            .collect(),
        None => Vec::new(),
    }
}

fn parse_since(s: Option<&str>) -> Option<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    match s? {
        "7d" => Some(now - 7 * 86_400),
        "30d" => Some(now - 30 * 86_400),
        _ => None, // "all" and anything unrecognised → no filter
    }
}

/// W1.A — the docs-gallery's read-state facet token. `NeverOpened` means
/// the artifact id is entirely ABSENT from the reading rollup map (never
/// opened AND no list read-override); the other three mirror
/// `kb_core::lists::ReadState` equality on a row that IS present. A row
/// present via an override alone (e.g. `read_override = "unread"` on a
/// never-opened artifact) matches `unread`, not `never-opened` — the two
/// are deliberately distinct facets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadToken {
    NeverOpened,
    Unread,
    InProgress,
    Read,
}

/// Parse the `read` csv into the set of tokens to keep. Unknown tokens are
/// dropped; an empty result means "no read-state filter". Mirrors
/// `routes::search::parse_read`, extended with `never-opened`.
fn parse_read(s: Option<&str>) -> Vec<ReadToken> {
    match s {
        Some(s) => s
            .split(',')
            .filter_map(|t| match t.trim() {
                "never-opened" => Some(ReadToken::NeverOpened),
                "unread" => Some(ReadToken::Unread),
                "in_progress" => Some(ReadToken::InProgress),
                "read" => Some(ReadToken::Read),
                _ => None,
            })
            .collect(),
        None => Vec::new(),
    }
}

/// One row's verdict under the `read` facet: kept iff ANY requested token
/// matches. An id absent from `rollup` only satisfies `NeverOpened`; a
/// present row is tested against the other three by `ReadState` equality
/// (never `NeverOpened`, which can't match a present row by definition).
fn read_token_matches(
    id: &str,
    rollup: &std::collections::HashMap<String, kb_core::reading::ReadRollup>,
    tokens: &[ReadToken],
) -> bool {
    use kb_core::lists::ReadState;
    match rollup.get(id) {
        Some(rr) => tokens.iter().any(|t| match t {
            ReadToken::NeverOpened => false,
            ReadToken::Unread => rr.state == ReadState::Unread,
            ReadToken::InProgress => rr.state == ReadState::InProgress,
            ReadToken::Read => rr.state == ReadState::Read,
        }),
        None => tokens.contains(&ReadToken::NeverOpened),
    }
}

/// W2.3a — parse + sanitise the `?ids=` csv into the `DocsQuery.ids` gate.
/// Ids outside the storage layer's own alnum/`-` allowlist (see
/// `storage::lance::get_by_ids`'s doc comment) are dropped rather than
/// rejected — a stray non-id token in a hand-edited URL simply can't match
/// any row. Returns `Err(detail)` only when the SANITISED set still exceeds
/// `MAX_IDS_FILTER` — a lasso of hundreds of docs shouldn't be silently
/// truncated to an arbitrary subset. Absent param or an all-empty/all-junk
/// csv both resolve to `Ok(None)` ("no id-set constraint"), matching
/// `DocsQuery.ids`'s own `None`-or-empty-means-unfiltered contract.
fn parse_ids_filter(s: Option<&str>) -> Result<Option<Vec<String>>, String> {
    let Some(s) = s else { return Ok(None) };
    let ids: Vec<String> = s
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .filter(|t| t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .map(str::to_string)
        .collect();
    if ids.len() > MAX_IDS_FILTER {
        return Err(format!(
            "ids: {} exceeds the {MAX_IDS_FILTER}-id cap",
            ids.len()
        ));
    }
    if ids.is_empty() {
        return Ok(None);
    }
    Ok(Some(ids))
}

// === CT-F4 — session residue ===============================================

/// Distinct, non-empty `kb-session` ids across a row set, in deterministic
/// (sorted) order — the input to the ONE batched `count_docs_by_kb_session`
/// read. Empty in, empty out: the caller then skips the fan-out entirely.
fn distinct_session_ids<'a, I>(docs: I) -> Vec<String>
where
    I: Iterator<Item = &'a kb_core::storage::lance::DocSummary>,
{
    let set: std::collections::BTreeSet<&str> = docs
        .filter_map(|d| d.kb_session.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    set.into_iter().map(str::to_string).collect()
}

/// CT-F4 — "how many memories were born in each of these sessions", summed
/// across the daemon's MEMORY-SCOPED corpora only (`[kb.*] memory_scope`,
/// the same population `routes::memory::memories_from` fans out over, and
/// the same per-corpus accessor `/api/sessions`' `memory_count` uses). A
/// daemon with no memory corpus does zero work.
///
/// One grouped count per corpus (`count_docs_by_kb_session` — ONE narrow
/// projection scan each, independent of how many ids are asked for), fanned
/// out concurrently in submission order (#28, `buffered_join`); a corpus
/// that errors folds to an empty map (its docs contribute 0) rather than
/// failing the whole gallery response. Counts are capture-independent
/// (#11 — `kb_session` is a doc column, not a capture row) and exclude
/// `memory-session` transcripts (MI-W4.0's recallable-memory population).
async fn residue_counts(
    state: &KbHandles,
    session_ids: Vec<String>,
) -> std::collections::HashMap<String, u64> {
    let mut out: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    if session_ids.is_empty() {
        return out;
    }
    let session_ids = &session_ids;
    let mut futs: Vec<crate::routes::CorpusFut<'_, std::collections::HashMap<String, u64>>> =
        Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        if ctx.memory_scope.is_none() {
            continue;
        }
        futs.push(Box::pin(async move {
            match ctx
                .storage
                .count_docs_by_kb_session(session_ids.clone())
                .await
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(
                        kb = %kb_name,
                        error = %e,
                        "count_docs_by_kb_session failed (residue)"
                    );
                    std::collections::HashMap::new()
                }
            }
        }));
    }
    if futs.is_empty() {
        return out;
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `crate::routes::FANOUT_CAP`).
    for per_kb in crate::routes::buffered_join(futs, state.fanout_cap).await {
        for (sid, n) in per_kb {
            *out.entry(sid).or_insert(0) += n;
        }
    }
    out
}

/// CT-F4 — the ONE arithmetic definition of a doc's residue, shared by the
/// wire field and the `?sort=residue` comparator so a rendered badge can
/// never disagree with the order it was sorted into.
///
/// `None` means "this doc has no birth session at all" — structurally
/// unrankable, not "zero" (the comparator sorts those LAST under either
/// `dir`). `Some(n)` counts the OTHER memories born in that session: when
/// the doc itself lives in a memory-scoped corpus it contributed 1 to its
/// own session's count, so that self-contribution is subtracted
/// (`self_counted`) — otherwise every memory would read as ≥1 "kept".
/// Transcripts (`memory-session`) are never in the counted population, so
/// they never self-subtract either.
fn residue_value(
    doc: &kb_core::storage::lance::DocSummary,
    counts: &std::collections::HashMap<String, u64>,
    self_counted: bool,
) -> Option<u32> {
    let sid = doc
        .kb_session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let mut n = counts.get(sid).copied().unwrap_or(0);
    if self_counted && kb_core::memory::is_recallable_memory_category(doc.kb_category.as_deref()) {
        n = n.saturating_sub(1);
    }
    Some(n.min(u32::MAX as u64) as u32)
}

/// The wire shape of [`residue_value`]: absent when zero (and absent when
/// the doc has no birth session), never a `0` badge.
fn session_residue_for(
    doc: &kb_core::storage::lance::DocSummary,
    counts: &std::collections::HashMap<String, u64>,
    self_counted: bool,
) -> Option<u32> {
    residue_value(doc, counts, self_counted).filter(|n| *n > 0)
}

/// CT-F4 — the `?sort=residue` comparator, the route-side sibling of
/// `docs_query::cmp_rows` (same `group`-folder prefix, same
/// deterministic-tie-break discipline).
///
/// Order: folder ASC when grouped → residue by `dir` (DESC by default:
/// most-kept first) → docs with NO birth session, always last regardless of
/// `dir` (they have nothing to rank, and reading "asc" as "0 first" would
/// bury every session-bearing row under the corpus's untouched majority) →
/// the DEFAULT row-local order (`cmp_rows`' recent DESC, id ASC) as the
/// tie-break, so paging is stable across requests.
fn cmp_residue(
    a: &DocRow,
    b: &DocRow,
    group: GroupKey,
    dir: SortDir,
    counts: &std::collections::HashMap<String, u64>,
    self_counted: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if group == GroupKey::Folder {
        let folder_ord = a.folder.cmp(&b.folder);
        if folder_ord != Ordering::Equal {
            return folder_ord;
        }
    }
    let va = residue_value(&a.doc, counts, self_counted);
    let vb = residue_value(&b.doc, counts, self_counted);
    let primary = match (va, vb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => match dir {
            SortDir::Asc => x.cmp(&y),
            SortDir::Desc => y.cmp(&x),
        },
    };
    primary.then_with(|| {
        cmp_rows(
            a,
            b,
            // The folder prefix (if any) is already equal at this point;
            // re-applying it here would be a no-op.
            GroupKey::None,
            SortKey::default(),
            SortKey::default().default_dir(),
        )
    })
}

fn build_response(
    row: DocRow,
    edge_counts: &std::collections::HashMap<String, (u32, u32)>,
    projection: Projection,
    source_root: &std::path::Path,
    read_rollup: Option<&std::collections::HashMap<String, kb_core::reading::ReadRollup>>,
) -> DocResponse {
    let (outlinks, backlinks) = edge_counts.get(&row.doc.id).copied().unwrap_or((0, 0));
    let r = row.doc;
    let source_relative = doc_rel_path(&r.path, source_root);
    // Slim drops every gallery-card field; the route still always
    // emits id/title/path/folder + time + kb_category. kb_category is
    // load-bearing for the SPA's `isIndexPage` check (the detail
    // siblings popover needs to skip generated `index-page` artifacts
    // when picking the folder's landing page).
    let slim = projection == Projection::Slim;
    // W1.A — read-state decorates ONLY the `default` projection; slim/atlas
    // stay bare regardless of what the caller passed (defense in depth —
    // the route already skips the `reading_rollup_for_ids` call for those
    // projections, but this keeps `build_response` the single source of
    // truth for the nullability rule, same as the `slim` gates above).
    let (read_state, read_pct, last_opened_unix) = if slim || projection == Projection::Atlas {
        (None, None, None)
    } else {
        decorate_read_state(&r.id, read_rollup)
    };
    DocResponse {
        id: r.id,
        title: r.title,
        path: r.path,
        folder: row.folder,
        source_relative,
        kb_category: r.kb_category,
        atlas_x: r.atlas_x,
        atlas_y: r.atlas_y,
        atlas_cluster: r.atlas_cluster,
        mtime_unix: r.mtime_unix,
        indexed_at_unix: r.indexed_at_unix,
        created_unix: r.created_unix,
        // Filled by the list handler's page-scoped first_seen join (or
        // single_doc_response's single-id lookup).
        first_indexed_unix: None,
        summary: if slim { None } else { r.summary },
        svg_count: if slim { None } else { r.svg_count },
        has_canvas: if slim { None } else { r.has_canvas },
        has_form: if slim { None } else { r.has_form },
        has_animation: if slim { None } else { r.has_animation },
        has_details: if slim { None } else { r.has_details },
        has_math: if slim { None } else { r.has_math },
        has_drag: if slim { None } else { r.has_drag },
        js_loc: if slim { None } else { r.js_loc },
        css_loc: if slim { None } else { r.css_loc },
        table_count: if slim { None } else { r.table_count },
        code_block_count: if slim { None } else { r.code_block_count },
        word_count: if slim { None } else { r.word_count },
        longread: if slim { None } else { r.longread },
        backlinks: if slim { None } else { Some(backlinks) },
        outlinks: if slim { None } else { Some(outlinks) },
        read_state,
        read_pct,
        last_opened_unix,
        // CT-F4 — filled by the list handler's page-scoped residue join
        // (`default` projection only, absent-when-zero).
        session_residue: None,
        tags: if slim { Vec::new() } else { r.tags },
        kb_status: if slim { None } else { r.kb_status },
        kb_severity: if slim { None } else { r.kb_severity },
    }
}

/// W1.A — stamp the server-derived read-state fields from `rollup`.
/// Mirrors `routes::search::apply_read`: an id absent from the map is
/// Unread with no pct/last-opened. Only called for the `default`
/// projection — `build_response` skips it entirely (returning a fully
/// bare triple) for `slim`/`atlas`. `rollup = None` is the defensive
/// fallback if a future caller ever invokes this without a fetched map;
/// it degrades to "Unread, no pct/last-opened" rather than panicking.
fn decorate_read_state(
    id: &str,
    rollup: Option<&std::collections::HashMap<String, kb_core::reading::ReadRollup>>,
) -> (Option<String>, Option<u8>, Option<i64>) {
    let rollup = match rollup {
        Some(m) => m,
        None => return (Some("unread".to_string()), None, None),
    };
    match rollup.get(id) {
        Some(rr) => (
            Some(rr.state.as_str().to_string()),
            Some(rr.completion_pct),
            rr.last_opened_unix,
        ),
        None => (Some("unread".to_string()), None, None),
    }
}

/// Rebuild the query string for the Link rel=next header with the
/// offset bumped to the next page. We round-trip only the params that
/// were actually present on the request so the next URL stays small
/// and predictable.
fn rebuild_query(p: &ListParams, next_offset: u32) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(8);
    parts.push(format!("offset={next_offset}"));
    if let Some(l) = p.limit {
        parts.push(format!("limit={l}"));
    }
    if let Some(f) = &p.folder {
        parts.push(format!("folder={}", url_q(f)));
    }
    if p.folder_exact == Some(true) {
        parts.push("folder_exact=1".into());
    }
    if let Some(t) = &p.tags {
        parts.push(format!("tags={}", url_q(t)));
    }
    if let Some(c) = &p.caps {
        parts.push(format!("caps={}", url_q(c)));
    }
    if let Some(s) = &p.since {
        parts.push(format!("since={}", url_q(s)));
    }
    if let Some(r) = &p.read {
        parts.push(format!("read={}", url_q(r)));
    }
    if let Some(ids) = &p.ids {
        parts.push(format!("ids={}", url_q(ids)));
    }
    if p.index == Some(1) {
        parts.push("index=1".into());
    }
    if let Some(s) = p.sort {
        parts.push(format!("sort={}", s.as_str()));
    }
    if let Some(d) = p.dir {
        parts.push(format!("dir={}", dir_to_str(d)));
    }
    if let Some(g) = p.group {
        parts.push(format!("group={}", group_to_str(g)));
    }
    if let Some(proj) = p.projection {
        parts.push(format!("projection={}", projection_to_str(proj)));
    }
    if let Some(q) = &p.q {
        parts.push(format!("q={}", url_q(q)));
    }
    parts.join("&")
}

/// Minimal RFC 3986 unreserved-plus-slash percent-encoder. Cheaper to
/// inline than pull in `percent-encoding`; the Link rel=next values we
/// build (slugs, folder paths, csv tag lists) only ever contain
/// characters that this set already accepts, so the encoder almost
/// never fires — it's just defense against a folder name with a space
/// or stray `&`.
fn url_q(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/');
        if safe {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn sort_to_str(s: SortKey) -> &'static str {
    match s {
        SortKey::Recent => "recent",
        SortKey::Indexed => "indexed",
        SortKey::Created => "created",
        SortKey::Title => "title",
        SortKey::Words => "words",
    }
}

fn dir_to_str(d: SortDir) -> &'static str {
    match d {
        SortDir::Asc => "asc",
        SortDir::Desc => "desc",
    }
}

fn group_to_str(g: GroupKey) -> &'static str {
    match g {
        GroupKey::None => "none",
        GroupKey::Folder => "folder",
    }
}

fn projection_to_str(p: Projection) -> &'static str {
    match p {
        Projection::Slim => "slim",
        Projection::Default => "default",
        Projection::Atlas => "atlas",
    }
}

/// Query for `artifact_bytes`. `download=1` flips the response from an
/// inline `text/html` (browser renders it) to an attachment (browser
/// saves it) by adding a `Content-Disposition: attachment` header with
/// the source file's basename. Any other / absent value serves inline,
/// preserving the v0.4 `kb get --format html` behaviour.
#[derive(Debug, Default, Deserialize)]
pub struct ArtifactBytesQuery {
    #[serde(default)]
    pub download: Option<u8>,
}

/// v0.4 D2 — `GET /api/kb/{kb}/artifact/{id}` — read-only artifact
/// bytes. Replaces the Host-header trick the CLI's `kb get --format
/// html` would otherwise need to hit the artifact subdomain serve.
///
/// Applies the v0.3 outbound scrubbing layer on the same conditions as
/// the artifact subdomain serve (`looks_non_loopback` — TCP peer + XFF
/// rule per CLAUDE.md invariant 6). Auth gates the route, but auth and
/// scrub serve different threat models: an operator who opted into
/// `strip_kb_prompt = true` expects the prompt template to be stripped
/// for any authorized partner pulling bytes from a remote machine, not
/// just for browser fetches over the artifact subdomain.
///
/// `?download=1` adds a `Content-Disposition: attachment` header so the
/// SPA's download control saves the raw source instead of navigating to
/// a rendered page. On loopback no scrubbing runs, so the saved file is
/// the true on-disk source.
pub async fn artifact_bytes(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((kb, id)): Path<(String, String)>,
    Query(q): Query<ArtifactBytesQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let row = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => return error_to_problem_json(&kb_core::Error::NotFound(format!("doc {id}"))),
        Err(e) => return error_to_problem_json(&e),
    };
    let raw = match std::fs::read(&row.path) {
        Ok(b) => b,
        Err(e) => return error_to_problem_json(&kb_core::Error::Io(e)),
    };
    // Markdown sources render to a full HTML page first, so `kb get` and the
    // SPA download control receive the same rendered DOM the artifact subdomain
    // serves (not raw `.md` source mislabelled `text/html`). The scrub below
    // then still strips the inline kb-prompt template on non-loopback pulls.
    // SC5 — `ctx.ext_map`, not the hardcoded extension check, so a mapped
    // extension (e.g. `txt = "markdown"`) renders here too, matching the
    // artifact-subdomain serve path.
    let is_md = ctx.ext_map.is_markdown(std::path::Path::new(&row.path));
    let bytes: Vec<u8> = if is_md {
        match std::str::from_utf8(&raw) {
            Ok(s) => kb_core::markdown::render_page(s).into_bytes(),
            // A `.md` that isn't valid UTF-8 can't be rendered; refuse rather
            // than serve raw source mislabelled text/html (mirrors the
            // subdomain serve path). Only reachable via a post-index TOCTOU.
            Err(_) => {
                return error_to_problem_json(&kb_core::Error::BadRequest(
                    "markdown source is not valid UTF-8".into(),
                ))
            }
        }
    } else {
        raw
    };

    let scrubbed: Option<Vec<u8>> = match &ctx.outbound {
        Some(cache)
            if crate::scrub::looks_non_loopback(
                Some(peer.ip()),
                &headers,
                &state.origin.trusted_proxies,
            ) =>
        {
            match std::str::from_utf8(&bytes) {
                Ok(s) => Some(crate::scrub::scrub(s, cache).into_bytes()),
                // Non-UTF8 body — can't safely scrub regex/template rules; pass through.
                Err(_) => None,
            }
        }
        _ => None,
    };
    let final_bytes = scrubbed.unwrap_or(bytes);

    let mut resp = (StatusCode::OK, final_bytes).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // Security (deep-review P1 → P0 for productization): this route serves
    // artifact HTML inline on the *parent* (trusted SPA) origin — unlike the
    // sandboxed `<id>.artifacts.<suffix>` subdomain where interactive artifacts
    // are meant to render. Without these headers a hostile artifact navigated
    // to here would execute scripts in the trusted app origin (a stored-XSS /
    // sandbox-bypass vector once the corpus can contain untrusted content).
    //   - `nosniff`: the browser must honour the declared type, never sniff a
    //     mislabelled body into something active.
    //   - `Content-Security-Policy: sandbox`: forces the response into a unique
    //     opaque origin with scripts/forms/same-origin access DISABLED — the
    //     "origin isolation" half of the fix. This endpoint exists to hand raw
    //     bytes to `kb get` / the SPA download control (header-blind byte
    //     consumers), so neutering active content here costs nothing; anything
    //     that needs to *run* an artifact uses the isolated subdomain serve.
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    resp.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    resp.headers_mut().insert(
        "X-Kb-Artifact-Id",
        HeaderValue::from_str(&row.id).unwrap_or(HeaderValue::from_static("?")),
    );
    if q.download == Some(1) {
        let basename = std::path::Path::new(&row.path)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}.html", row.id));
        // Rendered markdown downloads as `.html` (the served body is HTML,
        // not `.md` source) — `foo.md`/`foo.markdown` → `foo.html`.
        let basename = if is_md {
            std::path::Path::new(&basename)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| format!("{stem}.html"))
                .unwrap_or(basename)
        } else {
            basename
        };
        if let Ok(v) = HeaderValue::from_str(&attachment_disposition(&basename)) {
            resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
        }
    }
    resp
}

/// Build a `Content-Disposition: attachment` value that's both
/// header-safe and Unicode-faithful. Emits an ASCII `filename="…"`
/// fallback (quotes/backslash/control/non-ASCII replaced with `_`) plus
/// an RFC 5987 `filename*=UTF-8''…` percent-encoded form carrying the
/// real name. The whole string is ASCII, so `HeaderValue::from_str`
/// never rejects it.
pub(crate) fn attachment_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' && !c.is_control() {
                c
            } else {
                '_'
            }
        })
        .collect();
    let ascii = if ascii.is_empty() {
        "download".to_string()
    } else {
        ascii
    };
    let mut ext = String::new();
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            ext.push(b as char);
        } else {
            ext.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::lists::ReadState;
    use kb_core::reading::ReadRollup;
    use kb_core::storage::lance::DocSummary;
    use std::collections::HashMap;

    fn doc(id: &str) -> DocSummary {
        DocSummary {
            id: id.into(),
            title: id.to_uppercase(),
            path: format!("/root/{id}.html"),
            ..Default::default()
        }
    }

    fn row(id: &str, folder: &str) -> DocRow {
        DocRow {
            doc: doc(id),
            folder: folder.into(),
        }
    }

    fn rollup_entry(state: ReadState, pct: u8, opened: Option<i64>) -> ReadRollup {
        ReadRollup {
            last_opened_unix: opened,
            completion_pct: pct,
            state,
        }
    }

    // --- W1.A: `read` facet parsing + set-membership --------------------

    #[test]
    fn parse_read_keeps_known_tokens_and_drops_unknown() {
        assert_eq!(parse_read(None), Vec::new());
        assert_eq!(
            parse_read(Some("never-opened,unread,in_progress,read,bogus")),
            vec![
                ReadToken::NeverOpened,
                ReadToken::Unread,
                ReadToken::InProgress,
                ReadToken::Read,
            ]
        );
        // whitespace-tolerant, like the sibling csv parsers in this file.
        assert_eq!(
            parse_read(Some(" read , in_progress ")),
            vec![ReadToken::Read, ReadToken::InProgress]
        );
    }

    // invariant:35 read-facet (docs-gallery filter grammar)
    #[test]
    fn read_token_never_opened_means_absent_from_rollup_not_unread_state() {
        let mut rollup: HashMap<String, ReadRollup> = HashMap::new();
        // "b" carries a list read-override (`unread`) but was never
        // actually opened — present in the map with state Unread, distinct
        // from "a" which is entirely absent (truly never-opened).
        rollup.insert("b".into(), rollup_entry(ReadState::Unread, 0, None));

        let never_opened = [ReadToken::NeverOpened];
        assert!(
            read_token_matches("a", &rollup, &never_opened),
            "absent id matches never-opened"
        );
        assert!(
            !read_token_matches("b", &rollup, &never_opened),
            "an override-only row is present in the map — it is NOT never-opened"
        );

        let unread = [ReadToken::Unread];
        assert!(
            !read_token_matches("a", &rollup, &unread),
            "absent id does not satisfy the unread STATE facet"
        );
        assert!(
            read_token_matches("b", &rollup, &unread),
            "the override row's state is Unread"
        );
    }

    #[test]
    fn read_token_in_progress_and_read_are_state_equality_on_present_rows() {
        let mut rollup: HashMap<String, ReadRollup> = HashMap::new();
        rollup.insert(
            "a".into(),
            rollup_entry(ReadState::InProgress, 40, Some(100)),
        );
        rollup.insert("b".into(), rollup_entry(ReadState::Read, 100, Some(200)));

        assert!(read_token_matches("a", &rollup, &[ReadToken::InProgress]));
        assert!(!read_token_matches("a", &rollup, &[ReadToken::Read]));
        assert!(read_token_matches("b", &rollup, &[ReadToken::Read]));
        assert!(!read_token_matches("b", &rollup, &[ReadToken::InProgress]));
        // Multiple tokens are OR'd.
        assert!(read_token_matches(
            "b",
            &rollup,
            &[ReadToken::NeverOpened, ReadToken::Read]
        ));
    }

    // --- W1.A: page-scoped read-state decoration -------------------------

    #[test]
    fn build_response_default_projection_decorates_from_the_supplied_rollup_only() {
        // Simulates the route's page-scoped join: `reading_rollup_for_ids`
        // returns a map holding ONLY the visible page's ids. A doc whose id
        // is simply absent — as any off-page corpus doc would be — still
        // resolves via the same "absent = unread" rule `apply_read` uses,
        // proving `build_response` needs nothing beyond whatever (small)
        // map the caller passes: the join is never corpus-wide.
        let mut rollup: HashMap<String, ReadRollup> = HashMap::new();
        rollup.insert(
            "a".into(),
            rollup_entry(ReadState::InProgress, 42, Some(1_700_000_000)),
        );

        let resp_a = build_response(
            row("a", ""),
            &HashMap::new(),
            Projection::Default,
            std::path::Path::new("/root"),
            Some(&rollup),
        );
        assert_eq!(resp_a.read_state.as_deref(), Some("in_progress"));
        assert_eq!(resp_a.read_pct, Some(42));
        assert_eq!(resp_a.last_opened_unix, Some(1_700_000_000));

        // "b" is off-page (absent from the scoped map) — decorates Unread,
        // no pct/timestamp, exactly like a truly-never-opened doc would.
        let resp_b = build_response(
            row("b", ""),
            &HashMap::new(),
            Projection::Default,
            std::path::Path::new("/root"),
            Some(&rollup),
        );
        assert_eq!(resp_b.read_state.as_deref(), Some("unread"));
        assert_eq!(resp_b.read_pct, None);
        assert_eq!(resp_b.last_opened_unix, None);
    }

    #[test]
    fn build_response_slim_and_atlas_projections_stay_bare() {
        let mut rollup: HashMap<String, ReadRollup> = HashMap::new();
        rollup.insert("a".into(), rollup_entry(ReadState::Read, 100, Some(1)));

        for projection in [Projection::Slim, Projection::Atlas] {
            let resp = build_response(
                row("a", ""),
                &HashMap::new(),
                projection,
                std::path::Path::new("/root"),
                Some(&rollup),
            );
            assert_eq!(
                resp.read_state, None,
                "{projection:?} must stay bare even when a rollup entry exists"
            );
            assert_eq!(resp.read_pct, None);
            assert_eq!(resp.last_opened_unix, None);
        }
    }

    #[test]
    fn build_response_default_projection_with_no_rollup_fetched_is_unread() {
        // Defensive fallback path (`read_rollup = None`) — the route never
        // actually takes this branch for `default` today, but the function
        // must still degrade sanely rather than panic.
        let resp = build_response(
            row("a", ""),
            &HashMap::new(),
            Projection::Default,
            std::path::Path::new("/root"),
            None,
        );
        assert_eq!(resp.read_state.as_deref(), Some("unread"));
        assert_eq!(resp.read_pct, None);
        assert_eq!(resp.last_opened_unix, None);
    }

    // --- W1.A: `rebuild_query` round-trips `read` -------------------------

    #[test]
    fn rebuild_query_round_trips_read_param() {
        let p = ListParams {
            read: Some("unread,in_progress".to_string()),
            limit: Some(50),
            ..Default::default()
        };
        let qs = rebuild_query(&p, 50);
        assert!(
            qs.contains("read=unread%2Cin_progress"),
            "read param round-trips (csv percent-encoded): {qs}"
        );
        assert!(qs.starts_with("offset=50"));
    }

    #[test]
    fn rebuild_query_omits_read_when_absent() {
        let p = ListParams {
            limit: Some(10),
            ..Default::default()
        };
        let qs = rebuild_query(&p, 10);
        assert!(!qs.contains("read="));
    }

    // --- W2.3a: `ids` filter parsing + `rebuild_query` round-trip --------

    #[test]
    fn parse_ids_filter_sanitises_and_drops_junk() {
        assert_eq!(parse_ids_filter(None).unwrap(), None);
        assert_eq!(parse_ids_filter(Some("")).unwrap(), None);
        assert_eq!(
            parse_ids_filter(Some("a1b2, c3-d4 , ; DROP TABLE, ")).unwrap(),
            Some(vec!["a1b2".to_string(), "c3-d4".to_string()]),
            "non-alnum/dash tokens are dropped, not rejected"
        );
    }

    #[test]
    fn parse_ids_filter_rejects_over_cap() {
        let csv = (0..(MAX_IDS_FILTER + 1))
            .map(|i| format!("id{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let err = parse_ids_filter(Some(&csv)).unwrap_err();
        assert!(
            err.contains(&format!("{}", MAX_IDS_FILTER + 1)),
            "detail names the over-cap count: {err}"
        );
    }

    #[test]
    fn parse_ids_filter_accepts_exactly_the_cap() {
        let csv = (0..MAX_IDS_FILTER)
            .map(|i| format!("id{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let ids = parse_ids_filter(Some(&csv)).unwrap().unwrap();
        assert_eq!(ids.len(), MAX_IDS_FILTER);
    }

    #[test]
    fn rebuild_query_round_trips_ids_param() {
        let p = ListParams {
            ids: Some("a1,b2".to_string()),
            limit: Some(50),
            ..Default::default()
        };
        let qs = rebuild_query(&p, 50);
        assert!(
            qs.contains("ids=a1%2Cb2"),
            "ids param round-trips (csv percent-encoded): {qs}"
        );
    }

    #[test]
    fn rebuild_query_omits_ids_when_absent() {
        let p = ListParams {
            limit: Some(10),
            ..Default::default()
        };
        let qs = rebuild_query(&p, 10);
        assert!(!qs.contains("ids="));
    }

    // --- CT-F4: session residue ------------------------------------------

    /// A row with a `kb-session` (and an optional category, so the
    /// self-subtraction rule can be exercised).
    fn row_sess(id: &str, sid: Option<&str>, category: Option<&str>, mtime: i64) -> DocRow {
        let mut d = doc(id);
        d.kb_session = sid.map(str::to_string);
        d.kb_category = category.map(str::to_string);
        d.mtime_unix = Some(mtime);
        DocRow {
            doc: d,
            folder: String::new(),
        }
    }

    fn counts(pairs: &[(&str, u64)]) -> HashMap<String, u64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn list_sort_parses_residue_and_every_core_token() {
        for (token, want) in [
            ("recent", ListSort::Core(SortKey::Recent)),
            ("indexed", ListSort::Core(SortKey::Indexed)),
            ("created", ListSort::Core(SortKey::Created)),
            ("title", ListSort::Core(SortKey::Title)),
            ("words", ListSort::Core(SortKey::Words)),
            ("residue", ListSort::Residue),
        ] {
            let got: ListSort = serde_json::from_str(&format!("\"{token}\"")).unwrap();
            assert_eq!(got, want, "sort={token}");
        }
        // Unknown tokens still fail the same way they did before ListSort
        // existed (serde unknown-variant → the route's 400), never silently
        // degrading to the default order.
        assert!(serde_json::from_str::<ListSort>("\"bogus\"").is_err());
    }

    /// invariant:35 — absent `?sort=` must keep today's order byte-identical.
    #[test]
    fn absent_sort_param_resolves_to_the_pre_ct_f4_key_and_dir() {
        let spec = ListSort::default();
        assert_eq!(spec.core(), SortKey::default());
        assert_eq!(spec.default_dir(), SortKey::default().default_dir());

        // …and the comparator the route picks for it is `cmp_rows` with
        // exactly those arguments: sorting a fixture set through the
        // default spec matches sorting it through the raw SortKey.
        let rows = [
            row_sess("a", None, None, 100),
            row_sess("b", Some("s1"), None, 300),
            row_sess("c", Some("s1"), None, 200),
        ];
        let mut via_spec: Vec<&DocRow> = rows.iter().collect();
        via_spec.sort_by(|x, y| {
            cmp_rows(
                x,
                y,
                GroupKey::None,
                spec.core(),
                ListSort::default().default_dir(),
            )
        });
        let mut via_raw: Vec<&DocRow> = rows.iter().collect();
        via_raw.sort_by(|x, y| {
            cmp_rows(
                x,
                y,
                GroupKey::None,
                SortKey::default(),
                SortKey::default().default_dir(),
            )
        });
        let ids = |v: &Vec<&DocRow>| v.iter().map(|r| r.doc.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&via_spec), ids(&via_raw));
        assert_eq!(ids(&via_spec), vec!["b", "c", "a"], "recent DESC, id ASC");
    }

    #[test]
    fn residue_value_is_absent_without_a_session_and_zero_is_absent_on_the_wire() {
        let c = counts(&[("s1", 3)]);
        // No kb-session at all → structurally unrankable.
        assert_eq!(
            residue_value(&row_sess("a", None, None, 0).doc, &c, false),
            None
        );
        // Blank/whitespace session meta reads the same way (a truncated
        // capture meta once emptied the join corpus-wide, #11).
        assert_eq!(
            residue_value(&row_sess("a", Some("  "), None, 0).doc, &c, false),
            None
        );
        // A session with no counted memories is Some(0) for the ORDER…
        assert_eq!(
            residue_value(&row_sess("b", Some("s2"), None, 0).doc, &c, false),
            Some(0)
        );
        // …but absent on the wire (never a "0 kept" badge).
        assert_eq!(
            session_residue_for(&row_sess("b", Some("s2"), None, 0).doc, &c, false),
            None
        );
        assert_eq!(
            session_residue_for(&row_sess("c", Some("s1"), None, 0).doc, &c, false),
            Some(3)
        );
    }

    #[test]
    fn residue_value_subtracts_the_docs_own_contribution_inside_a_memory_corpus() {
        let c = counts(&[("s1", 3)]);
        // A memory listed from its OWN memory-scoped corpus counted itself
        // in the grouped scan — "3 memories born in s1" means 2 OTHERS.
        assert_eq!(
            residue_value(&row_sess("m", Some("s1"), None, 0).doc, &c, true),
            Some(2)
        );
        // A transcript is never in the counted population (MI-W4.0), so it
        // never self-subtracts even inside the memory corpus.
        assert_eq!(
            residue_value(
                &row_sess("t", Some("s1"), Some("memory-session"), 0).doc,
                &c,
                true
            ),
            Some(3)
        );
        // An artifact in a NON-memory corpus never contributed at all.
        assert_eq!(
            residue_value(&row_sess("a", Some("s1"), None, 0).doc, &c, false),
            Some(3)
        );
        // Never underflows when a corpus errored out / the count is stale.
        assert_eq!(
            residue_value(&row_sess("m", Some("s9"), None, 0).doc, &counts(&[]), true),
            Some(0)
        );
    }

    #[test]
    fn cmp_residue_orders_by_count_then_falls_back_to_the_default_order() {
        let c = counts(&[("s1", 5), ("s2", 2), ("s3", 5)]);
        let rows = [
            row_sess("a", Some("s2"), None, 900), // 2 kept, newest
            row_sess("b", Some("s1"), None, 100), // 5 kept, oldest
            row_sess("c", Some("s3"), None, 500), // 5 kept, middle
        ];
        let mut v: Vec<&DocRow> = rows.iter().collect();
        v.sort_by(|x, y| cmp_residue(x, y, GroupKey::None, SortDir::Desc, &c, false));
        assert_eq!(
            v.iter().map(|r| r.doc.id.as_str()).collect::<Vec<_>>(),
            vec!["c", "b", "a"],
            "5-kept rows lead; within the tie the default recent-DESC order decides"
        );

        // `dir=asc` flips the count axis only.
        let mut v: Vec<&DocRow> = rows.iter().collect();
        v.sort_by(|x, y| cmp_residue(x, y, GroupKey::None, SortDir::Asc, &c, false));
        assert_eq!(
            v.iter().map(|r| r.doc.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "c", "b"]
        );
    }

    #[test]
    fn cmp_residue_puts_session_less_docs_last_under_both_directions() {
        let c = counts(&[("s1", 4)]);
        let rows = [
            row_sess("x", None, None, 999),       // no session, newest
            row_sess("y", Some("s1"), None, 100), // 4 kept
            row_sess("z", Some("s2"), None, 200), // 0 kept but session-borne
            row_sess("w", None, None, 50),        // no session, oldest
        ];
        for dir in [SortDir::Desc, SortDir::Asc] {
            let mut v: Vec<&DocRow> = rows.iter().collect();
            v.sort_by(|a, b| cmp_residue(a, b, GroupKey::None, dir, &c, false));
            let ids: Vec<&str> = v.iter().map(|r| r.doc.id.as_str()).collect();
            assert_eq!(
                &ids[2..],
                &["x", "w"],
                "session-less rows trail under dir={dir:?} (newest-first among themselves)"
            );
            assert!(
                ids[..2].contains(&"y") && ids[..2].contains(&"z"),
                "both session-borne rows lead under dir={dir:?}: {ids:?}"
            );
        }
    }

    #[test]
    fn cmp_residue_keeps_the_folder_group_prefix() {
        let c = counts(&[("s1", 9)]);
        let mut hi = row_sess("hi", Some("s1"), None, 100);
        hi.folder = "zzz".into();
        let mut lo = row_sess("lo", Some("s2"), None, 100);
        lo.folder = "aaa".into();
        let rows = [hi, lo];
        let mut v: Vec<&DocRow> = rows.iter().collect();
        v.sort_by(|a, b| cmp_residue(a, b, GroupKey::Folder, SortDir::Desc, &c, false));
        assert_eq!(
            v.iter().map(|r| r.doc.id.as_str()).collect::<Vec<_>>(),
            vec!["lo", "hi"],
            "folder ASC still leads the comparator when group=folder"
        );
    }

    #[test]
    fn distinct_session_ids_dedupes_sorts_and_drops_blanks() {
        let rows = [
            row_sess("a", Some("s2"), None, 0),
            row_sess("b", Some("s1"), None, 0),
            row_sess("c", Some("s2"), None, 0),
            row_sess("d", None, None, 0),
            row_sess("e", Some("   "), None, 0),
        ];
        assert_eq!(
            distinct_session_ids(rows.iter().map(|r| &r.doc)),
            vec!["s1".to_string(), "s2".to_string()]
        );
        assert!(distinct_session_ids(std::iter::empty()).is_empty());
    }

    /// SURFACED, NEVER SCORED: residue is a page-scoped decoration the LIST
    /// handler stamps after `build_response`, never a row field — so every
    /// other producer of a `DocResponse` (the single-doc GET, the by-path
    /// GET, every projection) leaves it bare and nothing in the
    /// filter/sort/rank path can read it off a row.
    #[test]
    fn build_response_and_single_doc_never_set_session_residue() {
        for projection in [Projection::Slim, Projection::Default, Projection::Atlas] {
            let resp = build_response(
                row_sess("a", Some("s1"), None, 10),
                &HashMap::new(),
                projection,
                std::path::Path::new("/root"),
                None,
            );
            assert_eq!(resp.session_residue, None, "{projection:?}");
        }
        let resp = single_doc_response(doc("a"), std::path::Path::new("/root"));
        assert_eq!(resp.session_residue, None);
    }

    #[test]
    fn rebuild_query_round_trips_sort_residue() {
        let p = ListParams {
            sort: Some(ListSort::Residue),
            limit: Some(20),
            ..Default::default()
        };
        assert!(rebuild_query(&p, 20).contains("sort=residue"));
        // …and the core tokens keep their pre-CT-F4 spelling.
        let p = ListParams {
            sort: Some(ListSort::Core(SortKey::Created)),
            ..Default::default()
        };
        assert!(rebuild_query(&p, 0).contains("sort=created"));
    }
}
