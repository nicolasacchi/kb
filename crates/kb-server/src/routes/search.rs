//! `GET /api/search?q=&mode=hybrid|semantic|keyword&kb=` (default `hybrid`
//! after v0.1 — topic 11 §B.1, this plan §"Decisions taken"). For
//! semantic/hybrid, the route embeds the query via the kb's Embedder
//! before calling lance. If the kb has no embedder configured (kb.toml
//! missing `embedding_model`), semantic/hybrid return 400.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

const DEFAULT_LIMIT: u32 = 20;

/// SQ2 — candidate-pool size per arm for Rust-side hybrid fusion. Each
/// arm (BM25, vector) is fetched at `max(limit, FUSION_POOL)` so a doc
/// ranked just past `limit` in one arm but strong in the other still
/// reaches fusion. lance fused at only `limit`; this lifts that ceiling.
const FUSION_POOL: usize = 150;

/// Q-track — deeper candidate pool when a metadata/read-state filter or a
/// non-relevance sort is active, so the post-filter/post-sort has enough
/// ranked rows to fill the page (a rare tag shouldn't return 3 hits just
/// because the top-150 were mostly other tags). Bounded by a constant, not
/// the request, so a hostile `?limit=` can't amplify the kNN work.
const FILTERED_POOL: u32 = 400;

/// SQ4 — how many fused candidates the cross-encoder reranker rescores.
/// The reranker runs a forward pass per candidate, so we cap it; the tail
/// below this keeps its fusion order.
const RERANK_CANDIDATES: usize = 50;

/// SQ5 — chunk over-fetch multiplier: to surface N distinct docs from the
/// chunk arm, fetch N×this chunks (a doc owns several near passages).
const CHUNK_OVERFETCH: u32 = 8;

/// Hard ceiling on the requested page size. Caps the candidate-pool and
/// chunk over-fetch work a single request can trigger (a hostile
/// `?limit=1000000` otherwise issues a million-row kNN, amplified 8× by
/// the chunk arm). A page far larger than this isn't a real UI need.
const SEARCH_MAX_LIMIT: u32 = 200;

/// Q-track (board B1) — cap on how many `history` 'open' rows a
/// read-during-window filter (`read_from`/`read_to`) scans per corpus. The
/// filter needs the FULL set of artifact ids opened in the window (not a
/// page), so this is wider than `history_opens_in_window`'s other caller
/// (the per-session readings route, 500) but still bounded: a window
/// matching more rows than this silently keeps only the newest
/// `READ_WINDOW_LIMIT` (the query is `ORDER BY started_at DESC`), biasing
/// toward recency — the same direction the feature already leans.
const READ_WINDOW_LIMIT: u32 = 5000;

/// Q-track (board B1) — match-context snippet extraction scans at most
/// this many chars of a body. Bodies can run to several MB; a match past
/// this point buys nothing (a snippet from deep in the doc is useless
/// context) and keeps the per-hit cost bounded regardless of corpus
/// content. Chars, not bytes — this module's contract is entirely
/// char-index based (see `extract_snippet`).
const SNIPPET_SCAN_CAP_CHARS: usize = 512 * 1024;

/// Q-track (board B1) — target snippet window width in chars, before
/// ellipses. Centered on the match; the actual returned window can be
/// narrower near either edge of the (possibly capped) body.
const SNIPPET_WINDOW_CHARS: usize = 240;

/// FF-B — one boxed per-corpus future for [`super::buffered_join`], carrying the
/// owning kb name alongside that corpus's arm result `A`. Boxed so the helper
/// takes a uniform `dyn Future` over the per-corpus closures.
type ArmFut<'a, A> =
    std::pin::Pin<Box<dyn std::future::Future<Output = (&'a KbName, A)> + Send + 'a>>;

#[derive(Debug, Deserialize)]
pub struct Params {
    pub q: String,
    /// `hybrid` (default) | `keyword` | `semantic`. Other values → 400.
    #[serde(default = "default_mode")]
    pub mode: String,
    pub kb: Option<String>,
    pub limit: Option<u32>,
    /// SQ3 — `one` (default) searches a single kb; `all` fans out across
    /// every kb on the daemon and merges results by cross-kb RRF.
    #[serde(default = "default_scope")]
    pub scope: String,
    /// SQ2c / Q-track — structured filters applied to the ranked candidate
    /// pool. `category` matches `kb-category` exactly; `folder` keeps hits
    /// whose source-relative path is in that folder (descendant-inclusive).
    /// Every column these read is already in `SEARCH_PROJECTION`, so the
    /// pool needs no widening — only deepening (`FILTERED_POOL`) so the
    /// post-filter doesn't starve the page.
    pub category: Option<String>,
    pub folder: Option<String>,
    /// Q-track facets (csv where multi-valued). `tags`/`exclude_tags` are
    /// any-of include / none-of exclude on the effective tag set;
    /// `status`/`severity` are any-of on `kb-status`/`kb-severity`; `caps`
    /// is all-of capability (`svg,interactive,code,longread`); `session`
    /// matches `kb_session` exactly. `since` is a relative window
    /// (`day|week|month|year`) over `since_field` (`created|modified`,
    /// default `modified`).
    pub tags: Option<String>,
    pub exclude_tags: Option<String>,
    pub status: Option<String>,
    pub severity: Option<String>,
    pub caps: Option<String>,
    pub since: Option<String>,
    pub since_field: Option<String>,
    pub session: Option<String>,
    /// Q-track read-state facet (csv of `unread|in_progress|read`, any-of)
    /// and sort menu. `sort` ∈ `relevance` (default) `|opened|modified|
    /// created|indexed|title|words|progress`; `dir` ∈ `asc|desc` (default
    /// per-key). Read-state + `opened`/`progress` lean on the reading
    /// rollup; the rest sort `DocSummary` columns.
    pub read: Option<String>,
    pub sort: Option<String>,
    pub dir: Option<String>,
    /// Q-track (board B1) — read-during window: keep only hits whose
    /// artifact has a `history` 'open' row with `started_at` inside
    /// `[read_from, read_to]` (either bound optional — an open end
    /// defaults to the epoch / now). Independent of `?read=` (current
    /// read STATE): this is "was it opened during this window", combined
    /// with an implicit AND. Unix seconds; the CLI accepts calendar dates
    /// and converts before the request reaches here.
    pub read_from: Option<i64>,
    pub read_to: Option<i64>,
    /// Q-track — reading-list membership filter: keep only hits whose
    /// artifact is an entry of this list (per-kb). Resolved to an
    /// artifact-id set via `list_entries_for_list`.
    pub list: Option<String>,
    /// Full-search-page opt-in (track F). `detail=full` widens each `Hit`
    /// with the gallery-card metadata (summary, tags, counts, capability
    /// flags) in one round-trip. Absent — the fast `Cmd+K` popup — and
    /// every hit serializes byte-identically to the slim contract.
    pub detail: Option<String>,
}

fn default_mode() -> String {
    "hybrid".to_string()
}

fn default_scope() -> String {
    "one".to_string()
}

/// Which timestamp the `since` window filters on.
#[derive(Clone, Copy)]
enum SinceField {
    Created,
    Modified,
}

/// Q-track — the search page's structured metadata filters, parsed once
/// from the query params and applied to the relevance-ranked candidate
/// pool (single-kb + per-corpus federated) and to the browse listing. The
/// drift-prone predicate leaves (tag fallback, folder descendant rule,
/// capability roll-up) are REUSED from `kb_core::docs_query`
/// (`tags_for`, `folder_matches`, `Capability::matches`) so search can't
/// drift from the gallery; the equality + date checks are inline. Read-
/// state is NOT here — it needs the per-kb reading rollup and is applied
/// by the caller (Q4).
struct Filters {
    category: Option<String>,
    folder: Option<String>,
    tags: Vec<String>,
    exclude_tags: Vec<String>,
    status: Vec<String>,
    severity: Vec<String>,
    caps: Vec<kb_core::docs_query::Capability>,
    since_unix: Option<i64>,
    since_field: SinceField,
    session: Option<String>,
}

impl Filters {
    fn from_params(p: &Params) -> Self {
        let norm = |s: &Option<String>| {
            s.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let since_field = match p.since_field.as_deref() {
            Some("created") => SinceField::Created,
            _ => SinceField::Modified,
        };
        Filters {
            category: norm(&p.category),
            folder: p
                .folder
                .as_deref()
                .map(|s| s.trim().trim_matches('/'))
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            tags: split_csv(p.tags.as_deref()),
            exclude_tags: split_csv(p.exclude_tags.as_deref()),
            status: split_csv(p.status.as_deref()),
            severity: split_csv(p.severity.as_deref()),
            caps: parse_caps(p.caps.as_deref()),
            since_unix: parse_since(p.since.as_deref()),
            since_field,
            session: norm(&p.session),
        }
    }

    /// True when any metadata filter is set — the signal to deepen the
    /// candidate pool so the post-filter doesn't starve the page.
    fn any(&self) -> bool {
        self.category.is_some()
            || self.folder.is_some()
            || !self.tags.is_empty()
            || !self.exclude_tags.is_empty()
            || !self.status.is_empty()
            || !self.severity.is_empty()
            || !self.caps.is_empty()
            || self.since_unix.is_some()
            || self.session.is_some()
    }

    /// Does this row pass every metadata filter? `source_path` is the kb's
    /// source root, needed to derive the row's folder (same derivation the
    /// gallery's `DocRow` uses).
    fn keep(&self, d: &kb_core::storage::lance::DocSummary, source_path: &std::path::Path) -> bool {
        use kb_core::docs_query::{folder_matches, tags_for};
        // R0 — session-transcript artifacts are an internal episodic store
        // (browsable via /api/sessions, semantically searchable via
        // `kb recollect`); they are NEVER document-search results. Drop them
        // unless the caller explicitly asked for them (`?category=memory-session`).
        // The kb's own gallery already excludes them at the lance scan; this is
        // the equivalent gate for the relevance-ranked search pool.
        if d.kb_category.as_deref() == Some(kb_core::sessions::MEMORY_SESSION_CATEGORY)
            && self.category.as_deref() != Some(kb_core::sessions::MEMORY_SESSION_CATEGORY)
        {
            return false;
        }
        if let Some(cat) = &self.category {
            if d.kb_category.as_deref() != Some(cat.as_str()) {
                return false;
            }
        }
        if let Some(fld) = &self.folder {
            let folder = kb_core::paths::doc_folder(&d.path, source_path);
            if !folder_matches(&folder, fld) {
                return false;
            }
        }
        if !self.tags.is_empty() || !self.exclude_tags.is_empty() {
            let row_tags = tags_for(d);
            if !self.tags.is_empty() && !row_tags.iter().any(|t| self.tags.iter().any(|q| q == t)) {
                return false;
            }
            if !self.exclude_tags.is_empty()
                && row_tags
                    .iter()
                    .any(|t| self.exclude_tags.iter().any(|x| x == t))
            {
                return false;
            }
        }
        // MI-W2.3 — a soft-forgotten memory (`kb-status: forgotten`, the
        // tombstone `DELETE …/artifacts/{id}` now writes by default) is
        // DELIBERATELY NOT auto-excluded here. `status` only ever narrows
        // when the caller explicitly asks for one (`?status=forgotten` or
        // any other value) — there is no default-exclude the way R0
        // hard-excludes `memory-session` above. This is a considered
        // choice, not an oversight: the entire point of a soft-forget
        // tombstone (vs. the old hard delete) is that the artifact still
        // EXISTS and stays findable — a trash can you can search, not a
        // silent hole. `recall` is the one surface that DOES drop it
        // (`rerank_with_policy_scored`'s `status == "forgotten"` filter,
        // invariant #10) because recall's whole contract is "current,
        // curated facts"; plain search's contract is "everything indexed
        // in this corpus", and a tombstoned memory is still indexed.
        if !self.status.is_empty() {
            match d.kb_status.as_deref() {
                Some(s) if self.status.iter().any(|q| q == s) => {}
                _ => return false,
            }
        }
        if !self.severity.is_empty() {
            match d.kb_severity.as_deref() {
                Some(s) if self.severity.iter().any(|q| q == s) => {}
                _ => return false,
            }
        }
        for cap in &self.caps {
            if !cap.matches(d) {
                return false;
            }
        }
        if let Some(session) = &self.session {
            if d.kb_session.as_deref() != Some(session.as_str()) {
                return false;
            }
        }
        if let Some(since) = self.since_unix {
            let t = match self.since_field {
                SinceField::Created => d.created_unix,
                SinceField::Modified => d.mtime_unix,
            };
            match t {
                Some(t) if t >= since => {}
                _ => return false,
            }
        }
        true
    }
}

/// Split a csv param into trimmed, non-empty tokens.
fn split_csv(s: Option<&str>) -> Vec<String> {
    match s {
        Some(s) => s
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    }
}

/// Parse a `caps` csv into capabilities, dropping unknown tokens. Mirrors
/// the gallery's `routes::docs::parse_caps`.
fn parse_caps(s: Option<&str>) -> Vec<kb_core::docs_query::Capability> {
    use kb_core::docs_query::Capability;
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

/// Resolve a relative `since` window to an absolute unix-seconds floor.
/// The search page's presets are `day|week|month|year` (the gallery uses
/// `7d|30d`); anything else (incl. "all") → no filter.
fn parse_since(s: Option<&str>) -> Option<i64> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    match s? {
        "day" => Some(now - 86_400),
        "week" => Some(now - 7 * 86_400),
        "month" => Some(now - 30 * 86_400),
        "year" => Some(now - 365 * 86_400),
        _ => None,
    }
}

/// Parse the `read` csv into the set of read-states to keep. Unknown
/// tokens are dropped; an empty result means "no read-state filter".
fn parse_read(s: Option<&str>) -> Vec<kb_core::lists::ReadState> {
    use kb_core::lists::ReadState;
    match s {
        Some(s) => s
            .split(',')
            .filter_map(|t| match t.trim() {
                "unread" => Some(ReadState::Unread),
                "in_progress" => Some(ReadState::InProgress),
                "read" => Some(ReadState::Read),
                _ => None,
            })
            .collect(),
        None => Vec::new(),
    }
}

/// Q-track — the search-page sort axis. `Relevance` is the score order the
/// arms already produce (and the only one the rerank touches); the rest
/// re-order the filtered pool. `Opened`/`Progress` read the reading rollup.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchSort {
    Relevance,
    Opened,
    Modified,
    Created,
    Indexed,
    Title,
    Words,
    Progress,
}

impl SearchSort {
    fn parse(s: Option<&str>) -> Self {
        match s {
            Some("opened") => Self::Opened,
            Some("modified") => Self::Modified,
            Some("created") => Self::Created,
            Some("indexed") => Self::Indexed,
            Some("title") => Self::Title,
            Some("words") => Self::Words,
            Some("progress") => Self::Progress,
            _ => Self::Relevance,
        }
    }

    /// Default direction: title is A→Z (asc); everything else is
    /// newest/most-first (desc).
    fn default_desc(self) -> bool {
        !matches!(self, Self::Title)
    }
}

/// `None` sorts as the smallest value (matches `docs_query::cmp_rows` and
/// the SPA's `-Infinity`), so missing timestamps fall to the bottom on a
/// descending sort.
fn cmp_opt_time(a: Option<i64>, b: Option<i64>) -> std::cmp::Ordering {
    a.unwrap_or(i64::MIN).cmp(&b.unwrap_or(i64::MIN))
}

/// Compare two pool rows under a non-relevance sort. Mirrors
/// `docs_query::cmp_rows` for the `DocSummary` columns and reads the
/// reading rollup for `Opened`/`Progress`.
///
/// `a_key`/`b_key` are the rollup-map lookup keys AND the stable
/// tiebreak — separate from `a.id`/`b.id` because the single-kb caller and
/// the federated caller use different keyspaces over the SAME rollup-map
/// type: single-kb passes the plain id (unique within one corpus);
/// federated passes `hit_key(kb, id)` (ids collide across corpora, see
/// `hit_key`'s doc comment) so two corpora's same-id rows sort and
/// tie-break independently instead of colliding.
fn cmp_search(
    a: &kb_core::storage::lance::DocSummary,
    a_key: &str,
    b: &kb_core::storage::lance::DocSummary,
    b_key: &str,
    sort: SearchSort,
    desc: bool,
    rollup: &std::collections::HashMap<String, kb_core::reading::ReadRollup>,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let opened = |key: &str| rollup.get(key).and_then(|r| r.last_opened_unix);
    let progress = |key: &str| rollup.get(key).map(|r| r.completion_pct).unwrap_or(0);
    let ord = match sort {
        SearchSort::Relevance => Ordering::Equal,
        SearchSort::Modified => cmp_opt_time(a.mtime_unix, b.mtime_unix),
        SearchSort::Created => cmp_opt_time(a.created_unix, b.created_unix),
        SearchSort::Indexed => cmp_opt_time(a.indexed_at_unix, b.indexed_at_unix),
        SearchSort::Title => a.title.cmp(&b.title),
        SearchSort::Words => a.word_count.unwrap_or(0).cmp(&b.word_count.unwrap_or(0)),
        SearchSort::Opened => cmp_opt_time(opened(a_key), opened(b_key)),
        SearchSort::Progress => progress(a_key).cmp(&progress(b_key)),
    };
    let primary = if desc { ord.reverse() } else { ord };
    primary.then_with(|| a_key.cmp(b_key))
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub hits: Vec<Hit>,
    /// Total server time in ms from the start of the handler to the
    /// final response — includes embedding, index queries, and
    /// serialization. Pre-v0.14 this only covered the lance query
    /// (post-embed), which under-reported wall time by 50–100 ms.
    pub ms: u64,
    /// Wall-time of the query-embedding step in ms. `0` when the
    /// embedding came from the daemon's LRU cache, or when the search
    /// mode didn't need an embedding (mode=keyword).
    pub embed_ms: u64,
    /// True when the query-embedding LRU served this request.
    pub cache_hit: bool,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Default, Serialize)]
pub struct Hit {
    pub id: String,
    pub title: String,
    /// Absolute on-disk path (what the indexer stored).
    pub path: String,
    /// Track U — source-root-relative path; the SPA builds the
    /// `/a/<kb>/<source_relative>` permalink from this.
    pub source_relative: String,
    pub kb_category: Option<String>,
    pub kb_status: Option<String>,
    pub kb_severity: Option<String>,
    /// SQ1 — relevance score (higher = more relevant) carried from the
    /// lance arm's score/distance column. Additive: lets clients debug
    /// ranking; `null` when the backend surfaced no score.
    pub score: Option<f32>,
    /// Q-track (board B1) — match-context snippet: a window of the
    /// artifact's own body text centered on the query's first
    /// (case-insensitive) term match, whitespace-collapsed, ellipsis-
    /// marked when cut. `None` for a pure-semantic hit with no literal
    /// term match (a vector-only hit, or browse mode with no query at
    /// all) — the SPA falls back to `summary`. Populated on BOTH the slim
    /// popup and the rich (`detail=full`) payload, bounded to the
    /// returned page (`SEARCH_MAX_LIMIT`) via one extra body round-trip
    /// (`get_bodies_by_ids`); never widens the candidate pool.
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Q-track (board B1) — honest arm decomposition: this hit's 0-based
    /// rank within the BM25 arm's OWN result order, captured before RRF
    /// fusion overwrites `score` with the fused value. `None` when the
    /// mode never ran a keyword arm (`mode=semantic`) or the arm simply
    /// didn't return this hit.
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25_rank: Option<u32>,
    /// Same as `bm25_rank`, for the vector arm. Both are additive and
    /// independent — `?mode=keyword` never carries `vec_rank`,
    /// `?mode=semantic` never carries `bm25_rank`.
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vec_rank: Option<u32>,
    // --- Rich-hit metadata (track F — the full search page, `?detail=full`).
    // Populated ONLY when the flag is set; every field skips serialization
    // when None, so the fast-popup payload (no flag) stays byte-identical to
    // the slim contract. Field names + nullability mirror `DocResponse`
    // (`/api/kb/{kb}/docs/{id}`) so the SPA reuses its existing card
    // vocabulary; the `kb` field below is the template for this
    // absent-or-value wire shape (`ts(optional)` + `skip_serializing_if`).
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_count: Option<u32>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longread: Option<bool>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime_unix: Option<i64>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexed_at_unix: Option<i64>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_unix: Option<i64>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub svg_count: Option<u32>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_count: Option<u32>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_block_count: Option<u32>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_canvas: Option<bool>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_form: Option<bool>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_animation: Option<bool>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_details: Option<bool>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_math: Option<bool>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_drag: Option<bool>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub js_loc: Option<String>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub css_loc: Option<String>,
    /// Q-track — server-derived read-state (`unread|in_progress|read`) from
    /// the per-kb reading rollup; works in federated scope (where the
    /// SPA's single-kb `useReadingProgress` can't reach). `read_pct` is the
    /// scroll completion 0..100 for the reading chip; `last_opened_unix` is
    /// the most-recent open visit (backs the "opened …" chip + `sort=opened`).
    /// All three only populate when `detail=full` AND the artifact has been
    /// opened (or carries a list read-override), so the slim popup payload
    /// is untouched.
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_state: Option<String>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_pct: Option<u8>,
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_opened_unix: Option<i64>,
    /// SQ3 — corpus this hit came from. Omitted on single-kb
    /// (`scope=one`) responses (so they stay byte-identical); set on
    /// federated (`scope=all`) results so the client can build the
    /// `/a/<kb>/<source_relative>` permalink.
    /// ts: optional (absent-or-value on the wire), unlike the sibling
    /// Options above which serialize as explicit null.
    #[cfg_attr(feature = "ts-export", ts(optional))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kb: Option<String>,
}

/// Copy the rich gallery-card metadata off a `DocSummary` onto a `Hit`
/// when `detail=full` was requested; a no-op otherwise, so the fast-popup
/// payload stays byte-identical (every rich field skips serialization
/// while it's None). Shared by the single-kb and federated paths so the
/// two construction sites can never drift. The columns it reads are
/// projected by `Storage::SEARCH_PROJECTION` (lance); on an un-widened row
/// they decode to None and this stays a no-op.
fn apply_rich(hit: &mut Hit, r: &kb_core::storage::lance::DocSummary, rich: bool) {
    if !rich {
        return;
    }
    hit.summary = r.summary.clone();
    hit.tags = Some(r.tags.clone());
    hit.word_count = r.word_count;
    hit.longread = r.longread;
    hit.mtime_unix = r.mtime_unix;
    hit.indexed_at_unix = r.indexed_at_unix;
    hit.created_unix = r.created_unix;
    hit.svg_count = r.svg_count;
    hit.table_count = r.table_count;
    hit.code_block_count = r.code_block_count;
    hit.has_canvas = r.has_canvas;
    hit.has_form = r.has_form;
    hit.has_animation = r.has_animation;
    hit.has_details = r.has_details;
    hit.has_math = r.has_math;
    hit.has_drag = r.has_drag;
    hit.js_loc = r.js_loc.clone();
    hit.css_loc = r.css_loc.clone();
}

/// Q-track — stamp the server-derived read-state fields onto a `Hit` from
/// the reading rollup, when `rich`. A row absent from the rollup is Unread
/// (no pct / last-opened). No-op when not rich, so the slim Cmd+K payload
/// stays byte-identical.
fn apply_read(
    hit: &mut Hit,
    id: &str,
    rollup: &std::collections::HashMap<String, kb_core::reading::ReadRollup>,
    rich: bool,
) {
    if !rich {
        return;
    }
    match rollup.get(id) {
        Some(rr) => {
            hit.read_state = Some(rr.state.as_str().to_string());
            hit.read_pct = Some(rr.completion_pct);
            hit.last_opened_unix = rr.last_opened_unix;
        }
        None => hit.read_state = Some("unread".to_string()),
    }
}

/// Q-track (board B1) — per-arm rank map (id -> 0-based rank in THAT
/// arm's own result order), captured before RRF fusion overwrites
/// `DocSummary::score` with the fused value. Backs `Hit.bm25_rank`/
/// `vec_rank`.
fn rank_map(
    hits: &[kb_core::storage::lance::DocSummary],
) -> std::collections::HashMap<String, u32> {
    hits.iter()
        .enumerate()
        .map(|(i, d)| (d.id.clone(), i as u32))
        .collect()
}

/// Federated-only cross-corpus identity key. `DocSummary::id` is a hash of
/// the source-relative path (`kb_core::ids::ArtifactId::from_path`), NOT
/// globally unique — two kbs sharing a rel path (a shared root
/// `index.html` template is the common case) hash to the SAME id. Every
/// per-hit map `federated_search` builds ACROSS corpora (the merged read
/// rollup, `bm25_rank`/`vec_rank`, the snippet body map) keys on
/// `hit_key(kb, id)` rather than `id` alone, so a later corpus's same-id
/// row never silently overwrites an earlier corpus's entry. `':'` is a
/// safe separator: kb names are `[a-z0-9_-]+` (`KbName::new`) and never
/// contain it.
fn hit_key(kb: &str, id: &str) -> String {
    format!("{kb}:{id}")
}

/// Q-track (board B1) — tokenize a search query into snippet-matchable
/// terms: lowercased, >=2 chars, with DSL atoms (`tag:foo`,
/// `category:bar`, ... — any token containing `:`) dropped entirely,
/// since they're structured filters the body wouldn't literally contain.
fn snippet_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter(|t| !t.contains(':'))
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|t| t.chars().count() >= 2)
        .map(str::to_lowercase)
        .collect()
}

/// Truncate `body` to at most `cap_chars` chars, snapped to a char
/// boundary via `char_indices` (never a byte slice). Returns the slice
/// and whether it was actually shortened — the caller uses that to know
/// a trailing ellipsis is owed even when the snippet window itself
/// reaches the (capped) slice's own edge.
fn cap_to_chars(body: &str, cap_chars: usize) -> (&str, bool) {
    for (count, (i, _)) in body.char_indices().enumerate() {
        if count == cap_chars {
            return (&body[..i], true);
        }
    }
    (body, false)
}

/// Case-insensitive first-occurrence search for `term_lower` (already
/// lowercased) in `haystack`, returning the match's CHAR index (not a
/// byte offset — the caller re-derives byte offsets via `haystack`'s own
/// `char_indices`, so this never slices `haystack` directly).
///
/// Lowers `haystack` via `char::to_lowercase().next()` — a single-char
/// approximation. This keeps exact char-count parity between the lowered
/// copy and `haystack` (each char maps to exactly one char), which is
/// what lets a byte offset found in the lowered copy translate safely
/// back to a char index. It's exact for the shapes this module is tested
/// against (Latin scripts incl. accented Italian, emoji — case-neutral);
/// a handful of multi-char case expansions (e.g. Turkish İ) may miss a
/// match. Acceptable for a display snippet, which is never a ranking
/// signal.
fn find_ci_char_idx(haystack: &str, term_lower: &str) -> Option<usize> {
    if term_lower.is_empty() {
        return None;
    }
    let lowered: String = haystack
        .chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect();
    let byte_idx = lowered.find(term_lower)?;
    Some(lowered[..byte_idx].chars().count())
}

/// Collapse any run of whitespace (incl. newlines) to a single space and
/// trim the ends.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.trim().chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
            }
            last_was_space = true;
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out
}

/// Q-track (board B1) — extract a match-context snippet from `body` for
/// `query`, or `None` for a pure-semantic hit with no literal term match
/// (the SPA falls back to the doc summary). `body` is scanned only up to
/// `SNIPPET_SCAN_CAP_CHARS`; the returned window is ~`SNIPPET_WINDOW_CHARS`
/// wide, snapped to char boundaries (`char_indices` — never a byte
/// slice, so this is safe on multibyte text), whitespace-collapsed, with
/// a leading/trailing ellipsis whenever the window doesn't reach the
/// (possibly capped) body's edge.
fn extract_snippet(body: &str, query: &str) -> Option<String> {
    let terms = snippet_terms(query);
    if terms.is_empty() {
        return None;
    }
    let (scan, capped) = cap_to_chars(body, SNIPPET_SCAN_CAP_CHARS);
    // Earliest occurrence across every term — "the first occurrence of
    // any term", not just the first term that happens to match anywhere.
    let match_char = terms
        .iter()
        .filter_map(|t| find_ci_char_idx(scan, t))
        .min()?;

    let half = SNIPPET_WINDOW_CHARS / 2;
    let start_char = match_char.saturating_sub(half);
    let end_char = match_char + half;

    let mut start_byte = None;
    let mut end_byte = None;
    for (idx, (b, _)) in scan.char_indices().enumerate() {
        if idx == start_char {
            start_byte = Some(b);
        }
        if idx == end_char {
            end_byte = Some(b);
            break;
        }
    }
    let start_b = start_byte.unwrap_or(0);
    let end_b = end_byte.unwrap_or(scan.len());
    let collapsed = collapse_whitespace(&scan[start_b..end_b]);
    if collapsed.is_empty() {
        return None;
    }
    // Trailing ellipsis whenever the window ended before the (possibly
    // capped) scan slice's own end, OR the scan slice itself was capped
    // (so there's unconditionally more text, even if the window reached
    // the cap's edge).
    let trailing_cut = end_byte.is_some() || capped;
    let mut out = String::with_capacity(collapsed.len() + 8);
    if start_b > 0 {
        out.push('…');
        out.push(' ');
    }
    out.push_str(&collapsed);
    if trailing_cut {
        out.push(' ');
        out.push('…');
    }
    Some(out)
}

pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Query(params): Query<Params>,
) -> Response<Body> {
    if !matches!(params.mode.as_str(), "hybrid" | "keyword" | "semantic") {
        let err = kb_core::Error::BadRequest(format!(
            "unsupported mode {:?}; expected one of: hybrid, keyword, semantic",
            params.mode
        ));
        return error_to_problem_json(&err);
    }
    if !matches!(params.scope.as_str(), "one" | "all") {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "unsupported scope {:?}; expected one of: one, all",
            params.scope
        )));
    }
    // Capture before any fan-out (invariant #28) — identity is Attribution.
    let user = identity.user.clone();
    // SQ3 — federated fan-out. Branch BEFORE the single-kb resolution so
    // `scope=one` keeps its exact path (incl. the no-embedder→400 below).
    if params.scope == "all" {
        return federated_search(&state, &params, user).await;
    }

    // Resolve kb. If unspecified and exactly one kb is configured, use it.
    // Borrow `params.kb` (don't move it) so `Filters::from_params(&params)`
    // can still take a whole-struct borrow below.
    let kb_name: KbName = match params.kb.as_deref() {
        Some(s) => match KbName::new(s) {
            Ok(k) => k,
            Err(e) => return error_to_problem_json(&e),
        },
        None if state.kbs.len() == 1 => state.kbs.keys().next().unwrap().clone(),
        None => {
            let err = kb_core::Error::BadRequest(
                "must specify ?kb= when daemon serves multiple kbs".into(),
            );
            return error_to_problem_json(&err);
        }
    };

    let Some(ctx) = state.kbs.get(&kb_name) else {
        let err = kb_core::Error::NotFound(format!("kb {kb_name}"));
        return error_to_problem_json(&err);
    };

    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(SEARCH_MAX_LIMIT);
    // Track F — opt-in rich hit payload (the full search page).
    let rich = params.detail.as_deref() == Some("full");

    // SQ2c / Q-track — structured metadata filters over the candidate pool.
    let mut filters = Filters::from_params(&params);
    // R0-opt-in — a `?category`-less request against a kb configured with
    // `default_search_category` (e.g. the sessions corpus's
    // "memory-session") defaults `Filters.category` to it, so a user who
    // has directly scoped to that kb doesn't have to type/remember the
    // category every time. Fires ONLY on genuine absence — an explicit
    // `?category=` (any value, including "memory-session" itself) always
    // passes through `Filters::from_params` unchanged. `scope=all` never
    // reaches this branch (it returns via `federated_search` above), so R0
    // stays default-exclude for the federated path (architecture invariant
    // #11).
    if params.category.is_none() {
        if let Some(default_cat) = &ctx.default_search_category {
            filters.category = Some(default_cat.clone());
        }
    }
    let has_filter = filters.any();
    // Q-track — read-state facet + sort menu.
    let read_filter = parse_read(params.read.as_deref());
    let sort = SearchSort::parse(params.sort.as_deref());
    let desc = match params.dir.as_deref() {
        Some("asc") => false,
        Some("desc") => true,
        _ => sort.default_desc(),
    };
    // Q-track — reading-list membership filter: resolve the list's entries
    // to an artifact-id set once, then keep only hits in it.
    let list_ids: Option<std::collections::HashSet<String>> = match params
        .list
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(lid) => ctx
            .storage
            .list_entries_for_list(lid.to_string())
            .await
            .ok()
            .map(|es| es.into_iter().map(|e| e.artifact_id).collect()),
        None => None,
    };
    // Q-track (board B1) — read-during window: resolve to the set of
    // artifact ids with a `history` 'open' row inside [read_from, read_to]
    // once per request (an open bound defaults to the epoch / now). Reused
    // by both the browse and query paths below.
    let read_window_ids: Option<std::collections::HashSet<String>> =
        match (params.read_from, params.read_to) {
            (None, None) => None,
            (from, to) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                Some(
                    ctx.storage
                        .history_opens_in_window(
                            from.unwrap_or(0),
                            to.unwrap_or(now),
                            READ_WINDOW_LIMIT,
                        )
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|r| r.artifact_id)
                        .collect(),
                )
            }
        };
    // The reading rollup is needed to decorate rich cards, to apply the
    // read-state facet, and for the opened/progress sorts. The popup (no
    // rich, no read, relevance sort) never fetches it.
    let needs_rollup = rich
        || !read_filter.is_empty()
        || matches!(sort, SearchSort::Opened | SearchSort::Progress);
    // Deepen the pool whenever a filter, the read facet, the read-during
    // window, the list filter, or a non-relevance sort will thin/reorder it
    // before truncation — so the page isn't starved (hybrid over-fetches on
    // its own path regardless).
    let deepen = has_filter
        || list_ids.is_some()
        || read_window_ids.is_some()
        || !read_filter.is_empty()
        || sort != SearchSort::Relevance;
    let fetch_limit = if deepen {
        (limit).max(FILTERED_POOL)
    } else {
        limit
    };

    // Q-track — BROWSE mode. An empty query has no relevance signal, so skip
    // the embed + lance arms entirely and serve a filtered/sorted listing
    // (like the gallery), defaulting the sort to Modified. This makes the
    // search page a unified query+browse surface.
    if params.q.trim().is_empty() {
        let started = Instant::now();
        let rows = ctx.storage.list_docs(u32::MAX).await.unwrap_or_default();
        let rollup = if needs_rollup {
            ctx.storage
                .reading_rollup(user.clone())
                .await
                .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        let mut filtered: Vec<kb_core::storage::lance::DocSummary> = rows
            .into_iter()
            .filter(|r| filters.keep(r, &ctx.source_path))
            .filter(|r| list_ids.as_ref().is_none_or(|s| s.contains(&r.id)))
            .filter(|r| read_window_ids.as_ref().is_none_or(|s| s.contains(&r.id)))
            .collect();
        if !read_filter.is_empty() {
            filtered.retain(|r| {
                let st = rollup
                    .get(&r.id)
                    .map(|x| x.state)
                    .unwrap_or(kb_core::lists::ReadState::Unread);
                read_filter.contains(&st)
            });
        }
        // No relevance to preserve — Relevance falls back to Modified DESC.
        let (bsort, bdesc) = if sort == SearchSort::Relevance {
            (SearchSort::Modified, true)
        } else {
            (sort, desc)
        };
        filtered.sort_by(|a, b| cmp_search(a, &a.id, b, &b.id, bsort, bdesc, &rollup));
        let hits: Vec<Hit> = filtered
            .into_iter()
            .take(limit as usize)
            .map(|r| {
                let mut hit = Hit {
                    id: r.id.clone(),
                    title: r.title.clone(),
                    source_relative: kb_core::paths::doc_rel_path(&r.path, &ctx.source_path),
                    path: r.path.clone(),
                    kb_category: r.kb_category.clone(),
                    kb_status: r.kb_status.clone(),
                    kb_severity: r.kb_severity.clone(),
                    score: None,
                    kb: None,
                    ..Default::default()
                };
                apply_rich(&mut hit, &r, rich);
                apply_read(&mut hit, &r.id, &rollup, rich);
                hit
            })
            .collect();
        let ms = started.elapsed().as_millis() as u64;
        return Json(SearchResponse {
            hits,
            ms,
            embed_ms: 0,
            cache_hit: false,
        })
        .into_response();
    }

    // Start the wall-time clock here so `ms` reflects what the user
    // actually waits for, including the embedding step. Pre-v0.14 the
    // clock started AFTER `embed_one` returned, which made the
    // response look ~80 ms faster than it really was.
    let started = Instant::now();

    // Idempotent ensure for whichever index path we'll use. ensure_*
    // are cheap when the index already exists (per
    // kb_core::storage::lance findings on "Index already exists" → Ok).
    // Surface a real Err as problem+json rather than continuing into a
    // less-specific BM25/vector failure downstream.
    if let Err(e) = ctx.storage.ensure_fts_index().await {
        return error_to_problem_json(&e);
    }

    let needs_vector = matches!(params.mode.as_str(), "hybrid" | "semantic");
    let mut query_vec: Option<Vec<f32>> = None;
    let mut embed_ms: u64 = 0;
    let mut cache_hit: bool = false;
    if needs_vector {
        // Demand an embedder; if absent, surface a clean 400.
        let Some(emb) = &ctx.embedder else {
            let err = kb_core::Error::BadRequest(format!(
                "kb {kb_name} has no embedding_model configured; \
                 semantic + hybrid search require one. Run \
                 `kb model set <model> --kb {kb_name}` to enable."
            ));
            return error_to_problem_json(&err);
        };
        // SQ5 — ensure the index the vector arm will actually query.
        let ensure_vec = if ctx.chunked {
            ctx.storage.ensure_chunk_vector_index().await
        } else {
            ctx.storage.ensure_vector_index().await
        };
        if let Err(e) = ensure_vec {
            return error_to_problem_json(&e);
        }
        match crate::embed_cache::embed_query(&state.embed_cache, emb, &params.q).await {
            Ok(out) => {
                embed_ms = out.embed_ms;
                cache_hit = out.cache_hit;
                query_vec = Some(out.vec);
            }
            Err(e) => return error_to_problem_json(&e),
        }
        // TM-track — record the query-side embed stage (only on a real
        // embed; a cache hit did no model work).
        if !cache_hit {
            state
                .metrics
                .observe_search_stage(crate::state::SearchStage::Embed, embed_ms);
        }
    }

    // v0.7.1 P2 — match the mode AND the embedding together so the
    // semantic/hybrid arms bind the vector by pattern instead of
    // `query_vec.unwrap()`. The validation above guarantees a vector is
    // present for those modes, but a structural `unwrap()` in a request
    // handler is a panic waiting on a future refactor; the catch-all
    // arm returns a 400 instead.
    // TM-track — time the query stage in isolation (excludes embed) and
    // attribute it to the mode's storage path.
    let q_started = Instant::now();
    // Q-track (board B1) — per-arm rank maps for the honest arm
    // decomposition (`Hit.bm25_rank`/`vec_rank`). Populated inside
    // whichever mode arm(s) below actually ran; empty otherwise (e.g. a
    // pure `semantic` request never populates `bm25_rank_by_id`).
    let mut bm25_rank_by_id: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut vec_rank_by_id: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let (stage, result) = match (params.mode.as_str(), query_vec) {
        ("keyword", _) => {
            let r = ctx
                .storage
                .bm25_query(params.q.clone(), fetch_limit, ctx.typo_tolerance)
                .await;
            if let Ok(hits) = &r {
                bm25_rank_by_id = rank_map(hits);
            }
            (crate::state::SearchStage::Bm25, r)
        }
        ("semantic", Some(v)) => {
            let r = if ctx.chunked {
                ctx.storage
                    .chunk_vector_query(v, fetch_limit.saturating_mul(CHUNK_OVERFETCH), fetch_limit)
                    .await
            } else {
                ctx.storage.vector_query(v, fetch_limit).await
            };
            if let Ok(hits) = &r {
                vec_rank_by_id = rank_map(hits);
            }
            (crate::state::SearchStage::Vector, r)
        }
        ("hybrid", Some(v)) => {
            // SQ2 — fuse in Rust over an over-fetched candidate pool
            // instead of lance's blind top-`limit` RRF. Storage's own
            // `hybrid_query` is left untouched (memory recall depends on
            // it; root invariant #10/#11). Vector arm is passed first so
            // equal-score ties resolve the same way lance's
            // `[vector, fts]` reranker does.
            // Over-fetch at least FUSION_POOL; deepen to FILTERED_POOL when a
            // filter/sort will thin the pool before truncation.
            let pool = (fetch_limit as usize).max(FUSION_POOL) as u32;
            let vec_res = if ctx.chunked {
                ctx.storage
                    .chunk_vector_query(v, pool.saturating_mul(CHUNK_OVERFETCH), pool)
                    .await
            } else {
                ctx.storage.vector_query(v, pool).await
            };
            let bm_res = ctx
                .storage
                .bm25_query(params.q.clone(), pool, ctx.typo_tolerance)
                .await;
            let fused = match (vec_res, bm_res) {
                (Ok(vec_hits), Ok(bm_hits)) => {
                    // Q-track (board B1) — capture each arm's OWN rank
                    // before fusion overwrites `DocSummary::score` with
                    // the fused RRF value below.
                    vec_rank_by_id = rank_map(&vec_hits);
                    bm25_rank_by_id = rank_map(&bm_hits);
                    // Fuse the full over-fetched pool and apply the
                    // title-match boost. Filtering + truncation to `limit`
                    // happen uniformly after the match (SQ2c), so a boosted
                    // or filtered hit ranked past `limit` can still surface.
                    let mut f = kb_core::fusion::rrf_fuse(
                        vec![vec_hits, bm_hits],
                        kb_core::fusion::RRF_K,
                        pool as usize,
                    );
                    kb_core::fusion::apply_title_boost(
                        &mut f,
                        &params.q,
                        kb_core::fusion::TITLE_BOOST,
                    );
                    // GS-track — opt-in graph-degree boost, same placement
                    // contract as the title boost (full pool, pre-filter,
                    // pre-truncation). Non-relevance sorts re-order later
                    // and the SQ4 reranker overwrites scores, so the flag
                    // needs no extra gating here.
                    if let Some(w) = ctx.graph_boost {
                        let degrees = ctx.storage.edge_counts().await.unwrap_or_default();
                        kb_core::fusion::apply_graph_boost(&mut f, &degrees, w);
                    }
                    Ok(f)
                }
                (Err(e), _) | (_, Err(e)) => Err(e),
            };
            (crate::state::SearchStage::Hybrid, fused)
        }
        (mode, _) => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "search mode {mode:?} reached the query stage without an embedding"
            )));
        }
    };
    state
        .metrics
        .observe_search_stage(stage, q_started.elapsed().as_millis() as u64);
    let ms = started.elapsed().as_millis() as u64;

    let rows = match result {
        Ok(rows) => rows,
        Err(e) => return error_to_problem_json(&e),
    };
    // SQ2c / Q-track — apply structured metadata filters to the pool. The
    // folder rule now goes through `docs_query::folder_matches` (via
    // `Filters::keep`), so search folder filtering is identical to the
    // gallery's descendant-inclusive semantics.
    let mut filtered: Vec<kb_core::storage::lance::DocSummary> = rows
        .into_iter()
        .filter(|r| filters.keep(r, &ctx.source_path))
        .filter(|r| list_ids.as_ref().is_none_or(|s| s.contains(&r.id)))
        .filter(|r| read_window_ids.as_ref().is_none_or(|s| s.contains(&r.id)))
        .collect();
    // Q-track — read-state rollup backs the read facet, the opened/progress
    // sorts, and rich-card decoration. Scope it to the filtered candidate ids
    // (≤ FILTERED_POOL): the rollup is consumed only for these hits, so the
    // window scan stays proportional to the page instead of the whole
    // (append-only) history table. Empty (no fetch) when nothing needs it, so
    // the popup path does zero extra work.
    let rollup = if needs_rollup {
        let ids: Vec<String> = filtered.iter().map(|r| r.id.clone()).collect();
        ctx.storage
            .reading_rollup_for_ids(ids, user.clone())
            .await
            .unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };
    if !read_filter.is_empty() {
        filtered.retain(|r| {
            let st = rollup
                .get(&r.id)
                .map(|x| x.state)
                .unwrap_or(kb_core::lists::ReadState::Unread);
            read_filter.contains(&st)
        });
    }
    // SQ4 — rerank ONLY for relevance sort: a cross-encoder reorders by
    // relevance, which would fight an explicit date/title/read sort. The
    // rerank runs before truncation so a strong-but-low hit can be lifted.
    if sort == SearchSort::Relevance {
        if let Some(reranker) = &ctx.reranker {
            rerank_hits(reranker, &params.q, &mut filtered).await;
        }
    } else {
        filtered.sort_by(|a, b| cmp_search(a, &a.id, b, &b.id, sort, desc, &rollup));
    }
    // Truncate to the page size and project to the wire shape.
    let mut hits: Vec<Hit> = filtered
        .into_iter()
        .take(limit as usize)
        .map(|r| {
            // Clone the slim fields so `r` stays whole for `apply_rich`'s
            // borrow (the rich fields are read, not moved). ≤200 rows.
            let mut hit = Hit {
                id: r.id.clone(),
                title: r.title.clone(),
                source_relative: kb_core::paths::doc_rel_path(&r.path, &ctx.source_path),
                path: r.path.clone(),
                kb_category: r.kb_category.clone(),
                kb_status: r.kb_status.clone(),
                kb_severity: r.kb_severity.clone(),
                score: r.score,
                bm25_rank: bm25_rank_by_id.get(&r.id).copied(),
                vec_rank: vec_rank_by_id.get(&r.id).copied(),
                kb: None,
                ..Default::default()
            };
            apply_rich(&mut hit, &r, rich);
            apply_read(&mut hit, &r.id, &rollup, rich);
            hit
        })
        .collect();

    // Q-track (board B1) — match-context snippets, bounded to exactly the
    // returned page (already ≤ SEARCH_MAX_LIMIT via `limit`). One extra
    // body round-trip for just these ids — never widens the candidate
    // pool the way `detail=full`'s other fields do.
    if !params.q.trim().is_empty() {
        let ids: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        let body_by_id: std::collections::HashMap<String, String> = ctx
            .storage
            .get_bodies_by_ids(ids)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        for hit in &mut hits {
            hit.snippet = body_by_id
                .get(&hit.id)
                .and_then(|b| extract_snippet(b, &params.q));
        }
    }

    // Emit query event on the kb's bus.
    ctx.bus.emit(
        "query",
        serde_json::json!({
            "kb": kb_name.as_str(),
            "q": params.q,
            "mode": params.mode,
            "hits": hits.len(),
            "ms": ms,
            "embed_ms": embed_ms,
            "cache_hit": cache_hit,
        }),
    );

    Json(SearchResponse {
        hits,
        ms,
        embed_ms,
        cache_hit,
    })
    .into_response()
}

/// SQ3 — federated search across every kb on the daemon (`scope=all`).
/// Mirrors the memory-recall fan-out: per-corpus over-fetch, embed once
/// per model via the shared cache, keyword fallback for embedder-less
/// corpora, then a cross-kb Reciprocal Rank Fusion. Artifact ids are a
/// hash of the source-relative path (`ArtifactId::from_path`), NOT
/// globally unique — two kbs sharing a rel path (e.g. a shared root
/// `index.html` template) hash to the SAME id, so every cross-corpus
/// merge below (`rrf_fuse_keyed`, and the rollup/`bm25_rank`/`vec_rank`/
/// snippet-body maps via `hit_key`) is keyed on `(kb, id)`, never `id`
/// alone — otherwise a later corpus's same-id hit silently overwrites an
/// earlier corpus's entry instead of surfacing as its own row.
/// `scope=one` never reaches here.
async fn federated_search(state: &Arc<KbHandles>, params: &Params, user: String) -> Response<Body> {
    let started = Instant::now();
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).min(SEARCH_MAX_LIMIT);
    // Track F — opt-in rich hit payload (the full search page); same flag
    // as the single-kb path so federated cards carry the same metadata.
    let rich = params.detail.as_deref() == Some("full");
    // Q-track — structured metadata filters, applied per corpus before the
    // cross-kb merge (each corpus has its own source root for folder
    // derivation). Single-kb-only facets (tags/folder/session) are reset by
    // the SPA on scope=all, but the backend still honours any that arrive.
    let filters = Filters::from_params(params);
    let has_filter = filters.any();
    // Q-track — read-state facet + sort menu (whole-library, cross-corpus).
    let read_filter = parse_read(params.read.as_deref());
    let sort = SearchSort::parse(params.sort.as_deref());
    let desc = match params.dir.as_deref() {
        Some("asc") => false,
        Some("desc") => true,
        _ => sort.default_desc(),
    };
    let needs_rollup = rich
        || !read_filter.is_empty()
        || matches!(sort, SearchSort::Opened | SearchSort::Progress);
    // Q-track (board B1) — read-during window bounds (unix seconds). The
    // actual artifact-id SET is resolved per corpus below (each corpus's
    // `history` table is its own), but the [from, to] bounds are global.
    let read_window: Option<(i64, i64)> = match (params.read_from, params.read_to) {
        (None, None) => None,
        (from, to) => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Some((from.unwrap_or(0), to.unwrap_or(now)))
        }
    };
    let deepen = has_filter
        || !read_filter.is_empty()
        || read_window.is_some()
        || sort != SearchSort::Relevance;
    // Over-fetch per corpus so a strong hit ranked low in a busy corpus
    // survives the global merge (mirrors recall's per_corpus); deepen it
    // when a filter/sort will thin or reorder each arm before the merge.
    let per_corpus = if deepen {
        limit.saturating_mul(8).clamp(100, FILTERED_POOL)
    } else {
        limit.saturating_mul(4).clamp(20, 50)
    };

    // FF-B / DCB — arms are TAGGED (owning kb name) so `rrf_fuse_keyed`
    // dedups on (kb, id), never `id` alone (see the doc comment above).
    let mut arms: Vec<(String, Vec<kb_core::storage::lance::DocSummary>)> = Vec::new();
    let mut src_paths: std::collections::HashMap<String, std::path::PathBuf> =
        std::collections::HashMap::new();
    // Merged reading rollup across every corpus, keyed by `hit_key(kb,
    // id)` — NOT plain id (ids collide across corpora, see above) — so the
    // read facet, the opened/progress sorts, and rich-card decoration
    // never conflate two corpora's same-id docs.
    let mut rollup: std::collections::HashMap<String, kb_core::reading::ReadRollup> =
        std::collections::HashMap::new();
    let mut total_embed_ms: u64 = 0;
    let mut any_cache_hit = false;

    // Q-track — federated BROWSE (empty query): list+filter+sort across every
    // corpus, merged by the chosen sort (no relevance signal). Mirrors the
    // single-kb browse path; `list` ids are unique per kb so a corpus without
    // the list resolves to an empty set and drops out.
    if params.q.trim().is_empty() {
        // FF-B — fan the per-corpus list+filter out concurrently (bounded,
        // submission-ordered), then fold in BTreeMap order so the merge
        // order stays byte-identical to the serial path (invariant #28).
        // Each corpus future is a pure read (invariant 15).
        struct BrowseArm {
            source_path: std::path::PathBuf,
            rollup: Option<std::collections::HashMap<String, kb_core::reading::ReadRollup>>,
            rows: Vec<kb_core::storage::lance::DocSummary>,
        }
        let filters = &filters;
        let mut futs: Vec<ArmFut<'_, BrowseArm>> = Vec::new();
        for (name, ctx) in state.kbs.iter() {
            let user = user.clone();
            futs.push(Box::pin(async move {
                let rollup = if needs_rollup {
                    ctx.storage.reading_rollup(user).await.ok()
                } else {
                    None
                };
                let list_ids: Option<std::collections::HashSet<String>> = match params
                    .list
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(lid) => ctx
                        .storage
                        .list_entries_for_list(lid.to_string())
                        .await
                        .ok()
                        .map(|es| es.into_iter().map(|e| e.artifact_id).collect()),
                    None => None,
                };
                // Q-track (board B1) — this corpus's own read-during-window
                // id set (per-corpus `history` table; see the single-kb path).
                let read_window_ids: Option<std::collections::HashSet<String>> =
                    if let Some((from, to)) = read_window {
                        Some(
                            ctx.storage
                                .history_opens_in_window(from, to, READ_WINDOW_LIMIT)
                                .await
                                .unwrap_or_default()
                                .into_iter()
                                .filter_map(|r| r.artifact_id)
                                .collect(),
                        )
                    } else {
                        None
                    };
                let mut rows = Vec::new();
                for r in ctx.storage.list_docs(u32::MAX).await.unwrap_or_default() {
                    if !filters.keep(&r, &ctx.source_path) {
                        continue;
                    }
                    if list_ids.as_ref().is_some_and(|s| !s.contains(&r.id)) {
                        continue;
                    }
                    if read_window_ids.as_ref().is_some_and(|s| !s.contains(&r.id)) {
                        continue;
                    }
                    rows.push(r);
                }
                (
                    name,
                    BrowseArm {
                        source_path: ctx.source_path.clone(),
                        rollup,
                        rows,
                    },
                )
            }));
        }
        // PF-R1 — the operator-configurable `[server] fanout_cap` (default
        // 8, byte-identical to the old hardcoded `super::FANOUT_CAP`).
        let arms = super::buffered_join(futs, state.fanout_cap).await;
        // Each row is paired with its OWNING kb right here, at fold time —
        // no separate `id_to_kb` lookup map (and no first-wins collision
        // when two corpora share an id) is needed downstream.
        let mut merged: Vec<(String, kb_core::storage::lance::DocSummary)> = Vec::new();
        for (name, arm) in arms {
            let kb = name.to_string();
            src_paths.insert(kb.clone(), arm.source_path);
            if let Some(r) = arm.rollup {
                rollup.extend(r.into_iter().map(|(id, rr)| (hit_key(&kb, &id), rr)));
            }
            for r in arm.rows {
                merged.push((kb.clone(), r));
            }
        }
        if !read_filter.is_empty() {
            merged.retain(|(kb, r)| {
                let st = rollup
                    .get(&hit_key(kb, &r.id))
                    .map(|x| x.state)
                    .unwrap_or(kb_core::lists::ReadState::Unread);
                read_filter.contains(&st)
            });
        }
        let (bsort, bdesc) = if sort == SearchSort::Relevance {
            (SearchSort::Modified, true)
        } else {
            (sort, desc)
        };
        merged.sort_by(|(ak, a), (bk, b)| {
            cmp_search(
                a,
                &hit_key(ak, &a.id),
                b,
                &hit_key(bk, &b.id),
                bsort,
                bdesc,
                &rollup,
            )
        });
        merged.truncate(limit as usize);
        let hits: Vec<Hit> = merged
            .into_iter()
            .map(|(kb, r)| {
                let source_relative = src_paths
                    .get(&kb)
                    .map(|sp| kb_core::paths::doc_rel_path(&r.path, sp))
                    .unwrap_or_else(|| r.path.clone());
                let key = hit_key(&kb, &r.id);
                let mut hit = Hit {
                    id: r.id.clone(),
                    title: r.title.clone(),
                    source_relative,
                    path: r.path.clone(),
                    kb_category: r.kb_category.clone(),
                    kb_status: r.kb_status.clone(),
                    kb_severity: r.kb_severity.clone(),
                    score: None,
                    kb: Some(kb),
                    ..Default::default()
                };
                apply_rich(&mut hit, &r, rich);
                apply_read(&mut hit, &key, &rollup, rich);
                hit
            })
            .collect();
        let ms = started.elapsed().as_millis() as u64;
        return Json(SearchResponse {
            hits,
            ms,
            embed_ms: 0,
            cache_hit: false,
        })
        .into_response();
    }

    // FF-B — embed each distinct (model, query) ONCE up front (mirrors recall's
    // vec_by_model). Done before the fan-out so N corpora don't race N misses
    // through the shared QueryEmbedCache std-Mutex LRU. A federated request must
    // never 400 because one corpus lacks a model — corpora without an embedder
    // fall back to keyword inside the fan-out below.
    let wants_vector = matches!(params.mode.as_str(), "hybrid" | "semantic");
    let mut vec_by_model: std::collections::HashMap<String, Vec<f32>> =
        std::collections::HashMap::new();
    if wants_vector {
        for (_, ctx) in state.kbs.iter() {
            if let Some(emb) = &ctx.embedder {
                let model = emb
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .model_name()
                    .to_string();
                if let std::collections::hash_map::Entry::Vacant(slot) = vec_by_model.entry(model) {
                    if let Ok(out) =
                        crate::embed_cache::embed_query(&state.embed_cache, emb, &params.q).await
                    {
                        total_embed_ms += out.embed_ms;
                        any_cache_hit |= out.cache_hit;
                        slot.insert(out.vec);
                    }
                }
            }
        }
    }

    // FF-B — fan out the per-corpus search arms (bounded, submission-ordered),
    // then fold in BTreeMap order: src_paths/rollup always, the arm pushed
    // (tagged with its kb name) only on success (skip-on-error preserved). No
    // std::sync::Mutex guard is held across an await (invariant 15): the
    // embedder lock is released after reading model_name, before any await.
    struct HybridArm {
        source_path: std::path::PathBuf,
        rollup: Option<std::collections::HashMap<String, kb_core::reading::ReadRollup>>,
        hits: Option<Vec<kb_core::storage::lance::DocSummary>>,
        /// Q-track (board B1) — this corpus's own per-arm ranks, captured
        /// before fusion (empty when the mode never ran that arm here).
        bm25_rank: std::collections::HashMap<String, u32>,
        vec_rank: std::collections::HashMap<String, u32>,
    }
    let filters = &filters;
    let vec_by_model = &vec_by_model;
    let mut futs: Vec<ArmFut<'_, HybridArm>> = Vec::new();
    for (name, ctx) in state.kbs.iter() {
        let user = user.clone();
        futs.push(Box::pin(async move {
            // Fan-out: ensure Err skips this corpus (invariant #28 — one
            // corpus never 500s the fleet) rather than swallowing into a
            // less-specific query failure. Single-kb path surfaces via
            // error_to_problem_json above.
            if let Err(e) = ctx.storage.ensure_fts_index().await {
                tracing::warn!(kb = %name, error = %e, "federated search: ensure_fts_index failed; skipping corpus");
                return (
                    name,
                    HybridArm {
                        source_path: ctx.source_path.clone(),
                        rollup: None,
                        hits: None,
                        bm25_rank: std::collections::HashMap::new(),
                        vec_rank: std::collections::HashMap::new(),
                    },
                );
            }
            let query_vec: Option<Vec<f32>> = if wants_vector {
                if let Some(emb) = &ctx.embedder {
                    // SQ5 — ensure the index the vector arm queries.
                    let ensure_vec = if ctx.chunked {
                        ctx.storage.ensure_chunk_vector_index().await
                    } else {
                        ctx.storage.ensure_vector_index().await
                    };
                    if let Err(e) = ensure_vec {
                        tracing::warn!(kb = %name, error = %e, "federated search: ensure_vector_index failed; skipping corpus");
                        return (
                            name,
                            HybridArm {
                                source_path: ctx.source_path.clone(),
                                rollup: None,
                                hits: None,
                                bm25_rank: std::collections::HashMap::new(),
                                vec_rank: std::collections::HashMap::new(),
                            },
                        );
                    }
                    let model = emb
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .model_name()
                        .to_string();
                    vec_by_model.get(&model).cloned()
                } else {
                    None
                }
            } else {
                None
            };

            // Q-track (board B1) — this corpus's own per-arm ranks, captured
            // before fusion (mirrors the single-kb path).
            let mut bm25_rank: std::collections::HashMap<String, u32> =
                std::collections::HashMap::new();
            let mut vec_rank: std::collections::HashMap<String, u32> =
                std::collections::HashMap::new();
            let result = match (params.mode.as_str(), query_vec) {
                ("semantic", Some(v)) => {
                    let r = if ctx.chunked {
                        ctx.storage
                            .chunk_vector_query(
                                v,
                                per_corpus.saturating_mul(CHUNK_OVERFETCH),
                                per_corpus,
                            )
                            .await
                    } else {
                        ctx.storage.vector_query(v, per_corpus).await
                    };
                    if let Ok(h) = &r {
                        vec_rank = rank_map(h);
                    }
                    r
                }
                ("hybrid", Some(v)) => {
                    let vr = if ctx.chunked {
                        ctx.storage
                            .chunk_vector_query(
                                v,
                                per_corpus.saturating_mul(CHUNK_OVERFETCH),
                                per_corpus,
                            )
                            .await
                    } else {
                        ctx.storage.vector_query(v, per_corpus).await
                    };
                    let br = ctx
                        .storage
                        .bm25_query(params.q.clone(), per_corpus, ctx.typo_tolerance)
                        .await;
                    match (vr, br) {
                        (Ok(vh), Ok(bh)) => {
                            vec_rank = rank_map(&vh);
                            bm25_rank = rank_map(&bh);
                            let mut f = kb_core::fusion::rrf_fuse(
                                vec![vh, bh],
                                kb_core::fusion::RRF_K,
                                per_corpus as usize,
                            );
                            kb_core::fusion::apply_title_boost(
                                &mut f,
                                &params.q,
                                kb_core::fusion::TITLE_BOOST,
                            );
                            // GS-track — per-corpus graph boost, mirroring
                            // the single-kb site (each corpus boosts against
                            // its own in-degree map before the cross-kb RRF).
                            if let Some(w) = ctx.graph_boost {
                                let degrees = ctx.storage.edge_counts().await.unwrap_or_default();
                                kb_core::fusion::apply_graph_boost(&mut f, &degrees, w);
                            }
                            Ok(f)
                        }
                        (Err(e), _) | (_, Err(e)) => Err(e),
                    }
                }
                // keyword, or vector wanted but this corpus has no embedder.
                _ => {
                    let r = ctx
                        .storage
                        .bm25_query(params.q.clone(), per_corpus, ctx.typo_tolerance)
                        .await;
                    if let Ok(h) = &r {
                        bm25_rank = rank_map(h);
                    }
                    r
                }
            };

            let mut hits = match result {
                Ok(h) => h,
                // A corpus that errors is skipped, not fatal to the fan-out.
                Err(_) => {
                    return (
                        name,
                        HybridArm {
                            source_path: ctx.source_path.clone(),
                            // No hits from this corpus → none of its ids can be
                            // looked up in the merged rollup, so skip the scan.
                            rollup: None,
                            hits: None,
                            bm25_rank: std::collections::HashMap::new(),
                            vec_rank: std::collections::HashMap::new(),
                        },
                    );
                }
            };
            // R0 — `keep` ALWAYS runs here (the single-kb path already does):
            // its session-transcript exclusion is unconditional — a
            // memory-session row is never a document-search result unless the
            // caller opts in via ?category=memory-session. The pool was only
            // DEEPENED when has_filter/list/read/sort is set; `keep` itself is
            // O(1) per row when no explicit metadata filter is present, so
            // running it unconditionally costs nothing on the common path.
            hits.retain(|r| filters.keep(r, &ctx.source_path));
            // Per-corpus reading-list membership filter (list ids unique per kb).
            if let Some(lid) = params
                .list
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let ids: std::collections::HashSet<String> = ctx
                    .storage
                    .list_entries_for_list(lid.to_string())
                    .await
                    .map(|es| es.into_iter().map(|e| e.artifact_id).collect())
                    .unwrap_or_default();
                hits.retain(|r| ids.contains(&r.id));
            }
            // Q-track (board B1) — read-during window, this corpus's own
            // `history` table (mirrors the browse arm above).
            if let Some((from, to)) = read_window {
                let ids: std::collections::HashSet<String> = ctx
                    .storage
                    .history_opens_in_window(from, to, READ_WINDOW_LIMIT)
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|r| r.artifact_id)
                    .collect();
                hits.retain(|r| ids.contains(&r.id));
            }
            // Read-state rollup scoped to THIS corpus's surviving hit ids — the
            // merged rollup is only ever looked up by a fused hit id (all of
            // which came from some arm's hits), so scoping per arm keeps each
            // window scan proportional to the page, not the whole history table.
            let rollup = if needs_rollup {
                let ids: Vec<String> = hits.iter().map(|r| r.id.clone()).collect();
                ctx.storage.reading_rollup_for_ids(ids, user).await.ok()
            } else {
                None
            };
            (
                name,
                HybridArm {
                    source_path: ctx.source_path.clone(),
                    rollup,
                    hits: Some(hits),
                    bm25_rank,
                    vec_rank,
                },
            )
        }));
    }
    // Q-track (board B1) — global per-arm rank maps, merged across corpora.
    // Keyed on `hit_key(kb, id)` — a flat `.extend()` on plain id would
    // collide when two corpora's arms both rank a same-id hit.
    let mut bm25_rank_by_id: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    let mut vec_rank_by_id: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let arms_out = super::buffered_join(futs, state.fanout_cap).await;
    for (name, arm) in arms_out {
        let kb = name.to_string();
        src_paths.insert(kb.clone(), arm.source_path);
        if let Some(r) = arm.rollup {
            rollup.extend(r.into_iter().map(|(id, rr)| (hit_key(&kb, &id), rr)));
        }
        bm25_rank_by_id.extend(
            arm.bm25_rank
                .into_iter()
                .map(|(id, r)| (hit_key(&kb, &id), r)),
        );
        vec_rank_by_id.extend(
            arm.vec_rank
                .into_iter()
                .map(|(id, r)| (hit_key(&kb, &id), r)),
        );
        if let Some(hits) = arm.hits {
            arms.push((kb, hits));
        }
    }

    // Cross-kb RRF: each corpus is a TAGGED arm contributing 1/(K+rank) —
    // `rrf_fuse_keyed` dedups on (kb, id), so two corpora's same-id hits
    // never merge into one row (DCB / ARTIFACT HOST GRAMMAR v2: ids are
    // NOT globally unique). Fuse to a deeper pool when a read facet /
    // non-relevance sort will thin or reorder the merged list before
    // truncation to `limit`.
    let fuse_to = if deepen {
        (limit).max(FILTERED_POOL) as usize
    } else {
        limit as usize
    };
    let mut fused: Vec<(String, kb_core::storage::lance::DocSummary)> =
        kb_core::fusion::rrf_fuse_keyed(arms, kb_core::fusion::RRF_K, fuse_to);
    // Q-track — whole-library read-state facet + sort over the merged list.
    if !read_filter.is_empty() {
        fused.retain(|(kb, r)| {
            let st = rollup
                .get(&hit_key(kb, &r.id))
                .map(|x| x.state)
                .unwrap_or(kb_core::lists::ReadState::Unread);
            read_filter.contains(&st)
        });
    }
    if sort != SearchSort::Relevance {
        fused.sort_by(|(ak, a), (bk, b)| {
            cmp_search(
                a,
                &hit_key(ak, &a.id),
                b,
                &hit_key(bk, &b.id),
                sort,
                desc,
                &rollup,
            )
        });
    }
    fused.truncate(limit as usize);

    // Q-track (board B1) — match-context snippets, computed AFTER the
    // cross-kb merge + truncate so the body fetch is bounded to exactly
    // the ≤`limit` hits returned (never the deeper per-corpus candidate
    // pool). Grouped by originating corpus since each corpus's body lives
    // in its own lance table; fanned out per invariant #28 (never a
    // serial await loop over corpora). Keyed on `hit_key(kb, id)` so a
    // same-id body from a different corpus can never overwrite this one's.
    let mut body_by_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if !params.q.trim().is_empty() && !fused.is_empty() {
        let kb_by_name: std::collections::HashMap<&str, &crate::state::KbContext> =
            state.kbs.iter().map(|(k, v)| (k.as_str(), v)).collect();
        let mut ids_by_kb: std::collections::BTreeMap<&str, Vec<String>> =
            std::collections::BTreeMap::new();
        for (kb, r) in &fused {
            ids_by_kb.entry(kb.as_str()).or_default().push(r.id.clone());
        }
        let mut body_futs: Vec<super::CorpusFut<'_, Vec<(String, String)>>> = Vec::new();
        for (kb_name, ids) in ids_by_kb {
            if let Some(ctx) = kb_by_name.get(kb_name) {
                body_futs.push(Box::pin(async move {
                    ctx.storage
                        .get_bodies_by_ids(ids)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(id, body)| (hit_key(kb_name, &id), body))
                        .collect()
                }));
            }
        }
        // PF-R1 — the operator-configurable `[server] fanout_cap` (default
        // 8, byte-identical to the old hardcoded `super::FANOUT_CAP`).
        for pairs in super::buffered_join(body_futs, state.fanout_cap).await {
            body_by_id.extend(pairs);
        }
    }

    let hits: Vec<Hit> = fused
        .into_iter()
        .map(|(kb, r)| {
            let source_relative = src_paths
                .get(&kb)
                .map(|sp| kb_core::paths::doc_rel_path(&r.path, sp))
                .unwrap_or_else(|| r.path.clone());
            let key = hit_key(&kb, &r.id);
            let mut hit = Hit {
                id: r.id.clone(),
                title: r.title.clone(),
                source_relative,
                path: r.path.clone(),
                kb_category: r.kb_category.clone(),
                kb_status: r.kb_status.clone(),
                kb_severity: r.kb_severity.clone(),
                score: r.score,
                bm25_rank: bm25_rank_by_id.get(&key).copied(),
                vec_rank: vec_rank_by_id.get(&key).copied(),
                snippet: body_by_id
                    .get(&key)
                    .and_then(|b| extract_snippet(b, &params.q)),
                kb: Some(kb),
                ..Default::default()
            };
            apply_rich(&mut hit, &r, rich);
            apply_read(&mut hit, &key, &rollup, rich);
            hit
        })
        .collect();

    let ms = started.elapsed().as_millis() as u64;
    Json(SearchResponse {
        hits,
        ms,
        embed_ms: total_embed_ms,
        cache_hit: any_cache_hit,
    })
    .into_response()
}

/// SQ4 — reorder the top [`RERANK_CANDIDATES`] of `hits` in place using a
/// cross-encoder reranker, writing the rerank score onto each moved hit.
/// Runs the model in `spawn_blocking` (ONNX off the async threads) and
/// degrades to the input order on any error, so an unhealthy reranker
/// never breaks search. Document text is `title + body excerpt`.
async fn rerank_hits(
    reranker: &Arc<std::sync::Mutex<kb_core::embed_ipc::RerankerClient>>,
    query: &str,
    hits: &mut Vec<kb_core::storage::lance::DocSummary>,
) {
    let n = hits.len().min(RERANK_CANDIDATES);
    if n == 0 {
        return;
    }
    let texts: Vec<String> = hits[..n]
        .iter()
        .map(|h| match &h.summary {
            Some(s) if !s.is_empty() => format!("{}\n{}", h.title, s),
            _ => h.title.clone(),
        })
        .collect();
    let q = query.to_string();
    let reranker = reranker.clone();
    let ranked = tokio::task::spawn_blocking(move || {
        // Recover a poisoned lock rather than cascading panics: one prior
        // rerank panic must not permanently break this kb's search (we'd
        // still degrade to fusion order, but every later request would
        // log a panic). ONNX rerank holds no cross-call invariant.
        let mut g = reranker.lock().unwrap_or_else(|e| e.into_inner());
        g.rerank(&q, &texts, n)
    })
    .await;
    let order = match ranked {
        Ok(Ok(o)) if !o.is_empty() => o,
        _ => return, // degrade: keep the fusion order on any error
    };
    let mut reordered: Vec<kb_core::storage::lance::DocSummary> = Vec::with_capacity(hits.len());
    let mut used = vec![false; n];
    for (idx, score) in &order {
        if *idx < n && !used[*idx] {
            let mut d = hits[*idx].clone();
            d.score = Some(*score);
            reordered.push(d);
            used[*idx] = true;
        }
    }
    // Any top-N hit the reranker omitted keeps its place after the ranked set.
    for (i, u) in used.iter().enumerate() {
        if !*u {
            reordered.push(hits[i].clone());
        }
    }
    // The tail beyond the reranked window is untouched.
    reordered.extend_from_slice(&hits[n..]);
    *hits = reordered;
}

/// `GET /api/kb/{kb}/queries?limit=N` — v0.6 R1. Newest-first list of
/// recent search queries from the per-kb queries ring.
///
/// GC-B3 — `?zero_hit=true[&min_count=N]` switches the response to the
/// corpus-gap signal instead: every zero-hit query in the ring, grouped
/// by [`kb_core::history::normalize_query`], with occurrence counts
/// (`kb_core::history::ZeroHitGroup`). `limit` still applies, now as a
/// cap on the number of GROUPS returned (top-N by count) rather than
/// raw rows — the ring only holds `DEFAULT_CAPACITY` entries, so a
/// group count can never legitimately exceed it.
const QUERIES_DEFAULT_LIMIT: usize = 50;
const QUERIES_MAX_LIMIT: usize = kb_core::history::DEFAULT_CAPACITY;

#[derive(Debug, Deserialize)]
pub struct QueriesQuery {
    pub limit: Option<usize>,
    #[serde(default)]
    pub zero_hit: bool,
    pub min_count: Option<u64>,
}

pub async fn queries_list(
    State(state): State<Arc<KbHandles>>,
    Path(kb): Path<String>,
    Query(q): Query<QueriesQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let n = q
        .limit
        .unwrap_or(QUERIES_DEFAULT_LIMIT)
        .clamp(1, QUERIES_MAX_LIMIT);
    if q.zero_hit {
        let entries = ctx.queries.snapshot_all();
        let groups = kb_core::history::zero_hit_groups(&entries, q.min_count.unwrap_or(1));
        return Json(groups.into_iter().take(n).collect::<Vec<_>>()).into_response();
    }
    Json(ctx.queries.snapshot(n)).into_response()
}

/// `GET /api/queries/zero-hit?min_count=N&limit=M` — GC-B3, cross-kb
/// fan-out (invariant #28): every kb's zero-hit query groups, submission
/// order preserved (`state.kbs` is a `BTreeMap`), `buffered_join` bounds
/// concurrency, and a per-corpus panic/error never 500s the fleet view
/// (the future has no fallible step here, but the shape stays consistent
/// with every other scope=all handler).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct KbZeroHit {
    pub kb: String,
    pub groups: Vec<kb_core::history::ZeroHitGroup>,
}

pub async fn queries_zero_hit_all(
    State(state): State<Arc<KbHandles>>,
    Query(q): Query<QueriesQuery>,
) -> Response<Body> {
    let n = q
        .limit
        .unwrap_or(QUERIES_DEFAULT_LIMIT)
        .clamp(1, QUERIES_MAX_LIMIT);
    let min_count = q.min_count.unwrap_or(1);

    let mut futs: Vec<super::CorpusFut<'_, KbZeroHit>> = Vec::new();
    for (name, ctx) in &state.kbs {
        futs.push(Box::pin(async move {
            let entries = ctx.queries.snapshot_all();
            let groups = kb_core::history::zero_hit_groups(&entries, min_count);
            KbZeroHit {
                kb: name.to_string(),
                groups: groups.into_iter().take(n).collect(),
            }
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let kbs = super::buffered_join(futs, state.fanout_cap).await;
    Json(kbs).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kb_core::lists::ReadState;
    use kb_core::reading::ReadRollup;
    use kb_core::storage::lance::DocSummary;
    use std::collections::HashMap;
    use std::path::Path;

    fn doc(id: &str, path: &str) -> DocSummary {
        DocSummary {
            id: id.into(),
            title: id.into(),
            path: path.into(),
            ..Default::default()
        }
    }

    // A Params with everything defaulted — tests override single fields.
    fn params() -> Params {
        Params {
            q: String::new(),
            mode: default_mode(),
            kb: None,
            limit: None,
            scope: default_scope(),
            category: None,
            folder: None,
            tags: None,
            exclude_tags: None,
            status: None,
            severity: None,
            caps: None,
            since: None,
            since_field: None,
            session: None,
            read: None,
            sort: None,
            dir: None,
            read_from: None,
            read_to: None,
            list: None,
            detail: None,
        }
    }

    #[test]
    fn filters_tags_any_of_with_exclude() {
        let root = Path::new("/srv/kb");
        let mut d = doc("a", "/srv/kb/x.html");
        d.tags = vec!["rust".into(), "async".into()];

        let mut p = params();
        p.tags = Some("rust,go".into()); // any-of → matches on rust
        assert!(Filters::from_params(&p).keep(&d, root));

        p.tags = Some("python".into());
        assert!(!Filters::from_params(&p).keep(&d, root));

        // exclude wins even when include matches.
        let mut p = params();
        p.tags = Some("rust".into());
        p.exclude_tags = Some("async".into());
        assert!(!Filters::from_params(&p).keep(&d, root));
    }

    #[test]
    fn filters_status_severity_session_category() {
        let root = Path::new("/srv/kb");
        let mut d = doc("a", "/srv/kb/x.html");
        d.kb_status = Some("open".into());
        d.kb_severity = Some("high".into());
        d.kb_session = Some("sess-1".into());
        d.kb_category = Some("incident".into());

        let mut p = params();
        p.status = Some("open,closed".into());
        p.severity = Some("high".into());
        p.session = Some("sess-1".into());
        p.category = Some("incident".into());
        assert!(Filters::from_params(&p).keep(&d, root));

        let mut p = params();
        p.session = Some("sess-2".into());
        assert!(!Filters::from_params(&p).keep(&d, root));

        let mut p = params();
        p.status = Some("closed".into());
        assert!(!Filters::from_params(&p).keep(&d, root));
    }

    /// MI-W2.3 — a soft-forgotten (`kb-status: forgotten`) memory is NOT
    /// auto-excluded from a plain `kb search` (no `?status=` filter at
    /// all) — see the doc comment on this exact `if` in `keep`. The
    /// caller can still explicitly filter it OUT (`?status=<anything but
    /// forgotten>`) or IN (`?status=forgotten`), same as any other status
    /// value; there's no special-cased default-exclude the way R0
    /// hard-excludes `memory-session`.
    #[test]
    fn filters_forgotten_status_is_not_auto_excluded() {
        let root = Path::new("/srv/kb");
        let mut d = doc("a", "/srv/kb/x.html");
        d.kb_status = Some("forgotten".into());

        // No status filter at all → plain search still surfaces it.
        let p = params();
        assert!(Filters::from_params(&p).keep(&d, root));

        // An explicit opt-in filter also surfaces it.
        let mut p = params();
        p.status = Some("forgotten".into());
        assert!(Filters::from_params(&p).keep(&d, root));

        // An explicit filter for something else still excludes it, same
        // as any other status mismatch.
        let mut p = params();
        p.status = Some("open".into());
        assert!(!Filters::from_params(&p).keep(&d, root));
    }

    #[test]
    fn filters_caps_all_of_and_folder_descendant() {
        let root = Path::new("/srv/kb");
        let mut d = doc("a", "/srv/kb/pm/sub/incident.html");
        d.svg_count = Some(2);
        d.code_block_count = Some(1);

        // caps is all-of: both svg AND code must hold.
        let mut p = params();
        p.caps = Some("svg,code".into());
        assert!(Filters::from_params(&p).keep(&d, root));
        p.caps = Some("svg,interactive".into()); // interactive absent
        assert!(!Filters::from_params(&p).keep(&d, root));

        // folder is descendant-inclusive: "pm" matches "pm/sub/…".
        let mut p = params();
        p.folder = Some("pm".into());
        assert!(Filters::from_params(&p).keep(&d, root));
        p.folder = Some("other".into());
        assert!(!Filters::from_params(&p).keep(&d, root));
    }

    #[test]
    fn filters_since_field_picks_created_or_modified() {
        let root = Path::new("/srv/kb");
        let mut d = doc("a", "/srv/kb/x.html");
        d.mtime_unix = Some(2_000_000_000); // recent
        d.created_unix = Some(1_000); // ancient

        // since=year on modified → kept (mtime is recent).
        let mut p = params();
        p.since = Some("year".into());
        assert!(Filters::from_params(&p).keep(&d, root));

        // since=year on created → dropped (created is ancient).
        p.since_field = Some("created".into());
        assert!(!Filters::from_params(&p).keep(&d, root));
    }

    #[test]
    fn parse_read_and_search_sort() {
        assert_eq!(
            parse_read(Some("unread, read , bogus")),
            vec![ReadState::Unread, ReadState::Read]
        );
        assert!(parse_read(None).is_empty());
        assert!(matches!(
            SearchSort::parse(Some("opened")),
            SearchSort::Opened
        ));
        assert!(matches!(SearchSort::parse(None), SearchSort::Relevance));
        assert!(matches!(
            SearchSort::parse(Some("bogus")),
            SearchSort::Relevance
        ));
        assert!(!SearchSort::Title.default_desc());
        assert!(SearchSort::Modified.default_desc());
    }

    #[test]
    fn cmp_search_modified_title_and_rollup_axes() {
        use std::cmp::Ordering;
        let mut a = doc("a", "/k/a.html");
        let mut b = doc("b", "/k/b.html");
        a.mtime_unix = Some(100);
        b.mtime_unix = Some(200);
        a.word_count = Some(50);
        b.word_count = Some(10);

        let empty: HashMap<String, ReadRollup> = HashMap::new();
        // Modified desc → newer (b) first.
        assert_eq!(
            cmp_search(&a, &a.id, &b, &b.id, SearchSort::Modified, true, &empty),
            Ordering::Greater
        );
        // Words desc → more (a) first.
        assert_eq!(
            cmp_search(&a, &a.id, &b, &b.id, SearchSort::Words, true, &empty),
            Ordering::Less
        );
        // Title asc → "a" before "b".
        assert_eq!(
            cmp_search(&a, &a.id, &b, &b.id, SearchSort::Title, false, &empty),
            Ordering::Less
        );

        // opened/progress read the rollup; a missing row sorts last on desc.
        let mut roll = HashMap::new();
        roll.insert(
            "a".to_string(),
            ReadRollup {
                last_opened_unix: Some(500),
                completion_pct: 80,
                state: ReadState::InProgress,
            },
        );
        // a has an open time, b does not → a first on desc.
        assert_eq!(
            cmp_search(&a, &a.id, &b, &b.id, SearchSort::Opened, true, &roll),
            Ordering::Less
        );
        assert_eq!(
            cmp_search(&a, &a.id, &b, &b.id, SearchSort::Progress, true, &roll),
            Ordering::Less
        );
    }

    /// Federated-only: two colliding-id docs from different corpora get
    /// DISTINCT rollup lookups/tie-breaks when keyed by `hit_key(kb, id)`
    /// instead of the (shared) plain id.
    #[test]
    fn cmp_search_uses_explicit_keys_not_the_docs_shared_id() {
        use std::cmp::Ordering;
        // Same id, same mtime — a plain `a.id`-keyed rollup lookup couldn't
        // tell them apart at all; the composite key must.
        let a = doc("0eb547304658", "/alpha/index.html");
        let b = doc("0eb547304658", "/beta/index.html");
        let mut roll: HashMap<String, ReadRollup> = HashMap::new();
        roll.insert(
            hit_key("alpha", "0eb547304658"),
            ReadRollup {
                last_opened_unix: Some(500),
                completion_pct: 80,
                state: ReadState::InProgress,
            },
        );
        // alpha's copy has an open time; beta's (same doc id, no rollup row
        // under the beta-keyed key) does not → alpha first on desc.
        assert_eq!(
            cmp_search(
                &a,
                &hit_key("alpha", &a.id),
                &b,
                &hit_key("beta", &b.id),
                SearchSort::Opened,
                true,
                &roll,
            ),
            Ordering::Less
        );
        // The tie-break itself must also use the composite key (never
        // `a.id`/`b.id`, which are equal here and couldn't order them).
        assert_eq!(
            cmp_search(
                &a,
                &hit_key("alpha", &a.id),
                &b,
                &hit_key("beta", &b.id),
                SearchSort::Relevance,
                false,
                &HashMap::new(),
            ),
            "alpha:0eb547304658".cmp("beta:0eb547304658")
        );
    }

    // ---- Q-track (board B1) — match-context snippet extraction ----

    #[test]
    fn snippet_terms_drops_dsl_atoms_and_short_tokens() {
        assert_eq!(
            snippet_terms("borrow checker tag:rust a category:incident"),
            vec!["borrow".to_string(), "checker".to_string()]
        );
        // Punctuation-only edges are trimmed before the length check.
        assert_eq!(
            snippet_terms("\"borrow\", (checker)!"),
            vec!["borrow".to_string(), "checker".to_string()]
        );
        assert!(snippet_terms("tag:rust category:incident").is_empty());
        assert!(snippet_terms("").is_empty());
    }

    #[test]
    fn extract_snippet_returns_none_for_a_pure_semantic_hit() {
        // No term of the query literally appears in the body.
        assert!(extract_snippet("completely unrelated text here", "borrow checker").is_none());
        // A DSL-only query has no snippet-matchable terms at all.
        assert!(extract_snippet("the borrow checker runs at compile time", "tag:rust").is_none());
    }

    #[test]
    fn extract_snippet_finds_earliest_term_case_insensitively() {
        // Long filler on BOTH sides of the match (each well over the
        // ±120-char half-window) so the extracted window is cut on both
        // ends and owes both ellipses.
        let body = format!(
            "{}The BORROW checker runs at compile time.{}",
            "intro ".repeat(40),
            " outro".repeat(40)
        );
        let snip = extract_snippet(&body, "borrow").unwrap();
        assert!(
            snip.to_lowercase().contains("borrow"),
            "snippet must contain the match: {snip:?}"
        );
        assert!(snip.starts_with('…'), "expected leading ellipsis: {snip:?}");
        assert!(snip.ends_with('…'), "expected trailing ellipsis: {snip:?}");
    }

    #[test]
    fn extract_snippet_picks_the_earliest_of_several_terms() {
        // "checker" occurs early; "borrow" occurs 400+ chars later — well
        // outside the ~240-char window. The earliest OCCURRENCE across
        // every term wins (not the first-listed query word), so the
        // returned window covers "checker" and excludes "borrow" entirely.
        let filler = "y ".repeat(200);
        let body = format!("{}checker {filler}borrow end", "x".repeat(300));
        let snip = extract_snippet(&body, "borrow checker").unwrap();
        assert!(snip.to_lowercase().contains("checker"), "got: {snip:?}");
        assert!(
            !snip.to_lowercase().contains("borrow"),
            "borrow is far outside the window and must not appear: {snip:?}"
        );
    }

    #[test]
    fn extract_snippet_no_cut_omits_ellipsis() {
        // A short body entirely inside the window needs no ellipsis at all.
        let body = "short borrow checker body";
        let snip = extract_snippet(body, "borrow").unwrap();
        assert_eq!(snip, "short borrow checker body");
    }

    #[test]
    fn extract_snippet_collapses_whitespace() {
        let body = "before\n\n  borrow    checker\t\tafter";
        let snip = extract_snippet(body, "borrow").unwrap();
        assert!(!snip.contains('\n'), "got: {snip:?}");
        assert!(!snip.contains("  "), "double space survived: {snip:?}");
    }

    // Board B1 — multibyte Italian + emoji must never panic or split a
    // char, since the window cut is byte-index based internally but
    // snapped via `char_indices`.
    #[test]
    fn extract_snippet_multibyte_italian_and_emoji_stays_on_char_boundaries() {
        let body = "Perché l'articolo è così complesso? 🎉🎉🎉 il borrow checker è severissimo ma è giusto così, però à volte è frustrante 😅 e richiede pazienza".to_string();
        let snip = extract_snippet(&body, "borrow").unwrap();
        // Must be valid UTF-8 (a `String` guarantees this if constructed
        // without unsafe — the real assertion is that building it didn't
        // panic on a byte-boundary slice) and must contain the match.
        assert!(snip.to_lowercase().contains("borrow"), "got: {snip:?}");

        // A query term that only matches inside the emoji-adjacent text.
        let snip2 = extract_snippet(&body, "severissimo").unwrap();
        assert!(
            snip2.to_lowercase().contains("severissimo"),
            "got: {snip2:?}"
        );

        // Accented terms round-trip too (case-insensitive on Latin-1
        // supplement chars, no length-changing lowercasing involved).
        let snip3 = extract_snippet(&body, "PERCHÉ").unwrap();
        assert!(snip3.to_lowercase().contains("perché"), "got: {snip3:?}");
    }

    #[test]
    fn cap_to_chars_snaps_to_a_char_boundary_never_a_byte_slice() {
        // Each "🎉" is 4 bytes / 1 char — a byte-cap at an odd offset would
        // panic; the char-cap must not.
        let body = "🎉".repeat(10);
        let (capped, was_capped) = cap_to_chars(&body, 3);
        assert_eq!(capped.chars().count(), 3);
        assert!(was_capped);
        let (uncapped, was_capped2) = cap_to_chars(&body, 100);
        assert_eq!(uncapped, body);
        assert!(!was_capped2);
    }

    #[test]
    fn find_ci_char_idx_is_case_insensitive_and_returns_a_char_index() {
        assert_eq!(find_ci_char_idx("Hello World", "world"), Some(6));
        assert_eq!(find_ci_char_idx("Hello World", "xyz"), None);
        assert_eq!(find_ci_char_idx("", "a"), None);
        assert_eq!(find_ci_char_idx("a", ""), None);
    }

    #[test]
    fn collapse_whitespace_trims_and_merges_runs() {
        assert_eq!(collapse_whitespace("  a   b\n\nc\t d  "), "a b c d");
        assert_eq!(collapse_whitespace(""), "");
    }

    // ---- Q-track (board B1) — arm-decomposition rank map ----

    #[test]
    fn rank_map_indexes_by_position_not_id_order() {
        let hits = vec![doc("z", "/k/z.html"), doc("a", "/k/a.html")];
        let ranks = rank_map(&hits);
        assert_eq!(ranks.get("z"), Some(&0));
        assert_eq!(ranks.get("a"), Some(&1));
        assert_eq!(ranks.get("missing"), None);
    }

    // ---- Q-track (board B1) — read-during window param defaults ----

    #[test]
    fn params_read_window_defaults_to_none() {
        let p = params();
        assert!(p.read_from.is_none());
        assert!(p.read_to.is_none());
    }
}
