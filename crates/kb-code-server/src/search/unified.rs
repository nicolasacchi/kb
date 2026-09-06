//! W2.4 — the unified Search-Everywhere box: `GET /api/search?q=&repo=
//! &limit=` (`routes::search_unified`), one query fanned out to one or more
//! of the six search lanes via [`grammar::parse`], run CONCURRENTLY
//! (`tokio::join!`), and returned as FIXED, NEVER-INTERLEAVED sections —
//! [`run`] is the entrypoint, callable directly (no HTTP) for testing.
//!
//! # Response shape
//!
//! ```text
//! { "sections": [ { "lane": "files", "results": [...], "truncated": bool,
//!                    "unavailable_reason"?: string, "pending"?: bool },
//!                  ... ],
//!   "query_echo": "<the raw q, verbatim>" }
//! ```
//!
//! `sections` is always ordered per [`grammar::LANE_ORDER`] (files, symbols,
//! text, semantic, sessions, transcripts), REGARDLESS of prefix-parse order
//! — a lane the query's prefix didn't select is simply ABSENT from
//! `sections` (not present-with-an-empty-array). A lane that IS selected but
//! disabled/misconfigured/erroring gets `unavailable_reason` instead of
//! failing the whole box — every `run_*` fn below is infallible in its
//! return type (`-> LaneSection`, never `-> Result<...>`), matching
//! invariant #28's "drop-on-error, no `?`-propagate" spirit for this box's
//! own (much smaller, fixed-size, single-process) fan-out.
//!
//! # Empty query
//!
//! A `q` that is empty (or all whitespace) short-circuits BEFORE
//! [`grammar::parse`] ever runs: the response is exactly one section —
//! `files`, via [`super::FileIndex::recent`] — and nothing else is
//! attempted, not even as `unavailable_reason` noise. A prefix-selected
//! empty query (`"@"`, `"~~"`, ...) is different: [`grammar::parse`] still
//! resolves the prefix, and each lane's OWN empty-query rule applies at
//! execution time (files falls back to recents same as the no-prefix case;
//! every other lane reports an `unavailable_reason` of "q must not be
//! empty", matching that lane's standalone route's own 400).
//!
//! # Filters and repo scoping
//!
//! `lang:`/`path:` filters are applied by POST-FILTERING each lane's own
//! ranked hits (see [`matches_filters`]) — the files/symbols/text lane
//! implementations themselves are UNCHANGED (W2.1 scope), so this box
//! over-fetches ([`oversample_limit`]) before filtering + re-truncating,
//! rather than risking a filtered result set collapsing to near-nothing.
//! `repo:` (if present) overrides the endpoint's own `?repo=` for every
//! repo-scoped lane (files/symbols/text/semantic); `case:` only affects the
//! text lane. The text lane additionally needs exactly ONE resolved repo
//! (it has no multi-repo fan-out — see `search::text`'s own module doc): if
//! neither `repo:` nor `?repo=` narrows it to one and more than one repo is
//! configured, the text section reports `unavailable_reason` rather than
//! guessing.
//!
//! # Semantic staging
//!
//! The embed round-trip is real subprocess IPC, occasionally slow. Per the
//! design brief, the semantic lane races itself against a
//! [`SEMANTIC_STAGE_BUDGET`] (300ms) timeout: if it finishes in time, its
//! section is ordinary; if not, the section comes back immediately with
//! `pending: true` and EMPTY results (the in-flight embed/search future is
//! simply dropped, not cancelled-and-retried) — the client is expected to
//! re-query `GET /api/search/semantic` directly for that one lane. This
//! keeps the box's own soft ~2s response budget intact without the box
//! itself needing a top-level deadline: every OTHER lane is already
//! individually bounded (files/symbols/text are synchronous, in-process,
//! sub-millisecond at kb-repo scale per `search`'s own doc; sessions has its
//! own fixed 1.5s HTTP timeout, `sessions::TIMEOUT`; transcripts is a local
//! FTS5 query) — so the WORST case wall time across the whole
//! `tokio::join!` is the sessions lane's 1.5s, comfortably under budget,
//! achieved BY CONSTRUCTION rather than an explicit wrapping deadline.

use super::grammar::{self, Diagnostic, Filters, GroupKey, Lane, SortKey, TextMode, LANE_ORDER};
use super::results::{self, CountedSection, Facets, HitGroup};
use super::sessions;
use super::{Factors, LaneOpts, MAX_LIMIT};
use crate::config::KbDaemonSection;
use crate::lang;
use crate::rails::filter::Selection as RailsSelection;
use crate::routes;
use crate::semantic;
use crate::state::SharedState;
use crate::store::StoreBlocking;
use crate::transcripts;
use serde::Serialize;
use std::time::Duration;

/// How much wider than the caller's requested `limit` the files/symbols
/// lanes over-fetch before `lang:`/`path:` filtering — see the module doc.
const OVERSAMPLE: usize = 4;

/// The semantic lane's staged budget — see the module doc's "Semantic
/// staging" section.
const SEMANTIC_STAGE_BUDGET: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneSection {
    pub lane: &'static str,
    /// Lane-shaped hits — a JSON array whose element shape differs per
    /// lane (each lane's own `Serialize` impl, e.g. `FileHit`/`SymbolHit`/
    /// `sessions::SessionHit`), which is why this is a bare `Value` rather
    /// than one shared struct.
    pub results: serde_json::Value,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    /// `Some(true)` ONLY for a semantic section that missed its
    /// [`SEMANTIC_STAGE_BUDGET`] — see the module doc. Every other section
    /// omits this field entirely (`None`), never `Some(false)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<bool>,
    /// V71-D1 — present iff the query carried `explain:1` AND this lane can
    /// actually decompose its ranking (files/symbols today). See
    /// [`LaneExplain`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<LaneExplain>,
    /// V71-D2 — present iff the query carried a `group:` other than `none`.
    /// A PARTITION of `results` addressed by position — see
    /// [`super::results`]'s module doc. Absent (not empty) when ungrouped,
    /// so a client can tell "no grouping asked for" from "grouped into
    /// nothing".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<HitGroup>>,
    /// V72-I1 — what NARROWED this section, when something did: the Rails
    /// noun the query's `rails/1` facet atoms named, plus the honest count
    /// of files they matched (or the reason the atom was not applied).
    /// Absent on every unnarrowed section, so a response with no Rails atom
    /// is byte-identical to a pre-V72-I1 one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

/// V71-D1 — the section-level half of `explain:1`: what a hit's `rank`
/// MEANS in this box.
///
/// It says so explicitly because the honest answer is unusual: there is NO
/// fusion here. The six sections are fixed and never interleaved (design
/// D3), so a rank is a position within ONE lane and there is no cross-lane
/// additive score to report — the research's RRF caveat, respected rather
/// than papered over with an invented number. `factors_on` names the
/// factors that were ACTIVE for this run (`super::Factors`), so a reader
/// can tell "this factor contributed nothing" from "this factor was off".
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LaneExplain {
    /// Always `"per-lane"` — see the struct doc.
    pub rank_basis: &'static str,
    /// Always `"none"` in v1: sections are never interleaved, so nothing is
    /// fused and no single score spans lanes.
    pub fusion: &'static str,
    pub factors_on: Vec<&'static str>,
    pub note: &'static str,
}

/// The one place the "there is no fused score" sentence is written.
fn lane_explain(factors: &Factors) -> LaneExplain {
    let mut on: Vec<&'static str> = Vec::new();
    if factors.frecency {
        on.push("frecency");
    }
    if factors.demote_generated {
        on.push("demote_generated");
    }
    if factors.lexical_rarity {
        on.push("lexical_rarity");
    }
    LaneExplain {
        rank_basis: "per-lane",
        fusion: "none",
        factors_on: on,
        note: "sections are fixed and never interleaved: a rank is a position within \
               one lane, and there is no additive score across lanes to explain",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnifiedSearchResponse {
    pub sections: Vec<LaneSection>,
    /// The raw `q` exactly as received, unmodified by grammar parsing —
    /// lets a client confirm what was actually searched.
    pub query_echo: String,
    /// V71-D1 — kbcq/1's canonical re-rendering of the query that actually
    /// RAN (`grammar::normalize`): prefix, terms, and every filter the
    /// parser honoured, in one pasteable string. Empty for the
    /// empty-`q` recents short-circuit, which never parses anything.
    #[serde(default)]
    pub normalized: String,
    /// V71-D1 — the parser's non-fatal notes (unknown filter, bad value,
    /// unsupported negation), each naming the offending token and, where
    /// there is one, a did-you-mean. A query with diagnostics still RAN —
    /// the offending token was searched as an ordinary word.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    /// V71-D2 — the facet census over the sections above, present iff the
    /// query carried `facets:1`. Its counts are PAGE counts and the payload
    /// says so (`basis`/`note`) — see [`super::results`]'s module doc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facets: Option<Facets>,
    /// V71-D2 — the LLM contract's staleness half: what index generation
    /// answered this, and when. ALWAYS present (an agent must never have to
    /// infer freshness from a missing field).
    pub stale: Freshness,
}

/// V71-D2 — how fresh this answer is, in the only two terms this daemon can
/// state without lying.
///
/// `generation` is `Store::generation()` read as the response was assembled:
/// a monotonic counter this daemon bumps on every index mutation. Two
/// responses carrying the same value were computed over the same index; a
/// higher one means the tree moved in between. It is deliberately NOT a
/// commit distance — "behind by N commits" would need a git call per search
/// (the research's own `behind_commits` ask), and this box answers on every
/// keystroke.
///
/// The other half of staleness is per-hit and lives on the hit: V71-D2 adds
/// `blob_sha` to the FILES lane (the only lane whose index snapshot has the
/// blob in hand). The symbols/text/semantic/sessions/transcripts lanes do
/// not carry one, and this struct does not pretend otherwise — a named
/// follow-up, not a silently weaker promise.
///
/// A lane that could not run is NOT reported here: it already has
/// `unavailable_reason` on its own section, which is the research's
/// `refused: [{lane, reason}]` under the name this box has used since W2.4.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Freshness {
    pub generation: u64,
    /// RFC3339, UTC — when this response was assembled.
    pub as_of: String,
}

fn freshness(state: &SharedState) -> Freshness {
    Freshness {
        // A relaxed atomic load — safe to read straight from a tokio worker
        // (store.rs's 2026-08-31 incident is about SQLite calls, not this
        // counter).
        generation: state.store.generation(),
        as_of: chrono::Utc::now().to_rfc3339(),
    }
}

fn section(lane: Lane, results: serde_json::Value, truncated: bool) -> LaneSection {
    LaneSection {
        lane: lane.as_str(),
        results,
        truncated,
        unavailable_reason: None,
        pending: None,
        explain: None,
        groups: None,
        caption: None,
    }
}

fn unavailable(lane: Lane, reason: impl Into<String>) -> LaneSection {
    LaneSection {
        lane: lane.as_str(),
        results: serde_json::Value::Array(Vec::new()),
        truncated: false,
        unavailable_reason: Some(reason.into()),
        pending: None,
        explain: None,
        groups: None,
        caption: None,
    }
}

fn pending_section(lane: Lane) -> LaneSection {
    LaneSection {
        lane: lane.as_str(),
        results: serde_json::Value::Array(Vec::new()),
        truncated: false,
        unavailable_reason: None,
        pending: Some(true),
        explain: None,
        groups: None,
        caption: None,
    }
}

fn to_value<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

/// `true` if `path` passes BOTH `filters.path` (case-insensitive substring)
/// and `filters.lang` (exact match against `lang::detect`'s id, case-
/// insensitive) — either filter absent trivially passes. Shared by the
/// files/symbols/text lanes' post-filtering (see the module doc).
fn matches_filters(path: &str, filters: &Filters) -> bool {
    let lower = path.to_lowercase();
    if let Some(want_path) = &filters.path {
        if !lower.contains(&want_path.to_lowercase()) {
            return false;
        }
    }
    if let Some(want_lang) = &filters.lang {
        match lang::detect(path, None) {
            Some(info) if info.id.eq_ignore_ascii_case(want_lang) => {}
            _ => return false,
        }
    }
    // V71-D1 (kbcq/1) — `ext:` is a suffix test on the path itself, not a
    // `lang::detect` lookup: an extension with no tree-sitter grammar
    // (`.erb`, `.yml`, `.md`) is exactly the case a Rails operator reaches
    // for it FOR, and routing it through `lang` would silently drop them.
    if !filters.ext.is_empty() && !filters.ext.iter().any(|e| has_ext(&lower, e)) {
        return false;
    }
    // Exclusions beat inclusions — a candidate matching any `not_*` entry is
    // dropped even when it also matched the positive filter.
    if filters
        .not_path
        .iter()
        .any(|p| lower.contains(&p.to_lowercase()))
    {
        return false;
    }
    if filters.not_ext.iter().any(|e| has_ext(&lower, e)) {
        return false;
    }
    if !filters.not_lang.is_empty() {
        if let Some(info) = lang::detect(path, None) {
            if filters
                .not_lang
                .iter()
                .any(|l| info.id.eq_ignore_ascii_case(l))
            {
                return false;
            }
        }
    }
    true
}

/// `ext:rb` matches `app/models/order.rb`; `ext:min.js` matches
/// `app.min.js`. A leading dot on the FILTER is tolerated (`ext:.rb`) since
/// it is the obvious thing to type. `path_lower` must already be
/// lowercased; `want` is lowercased here (it comes straight off the query,
/// once per candidate — the ext list is single-digit-short by construction).
fn has_ext(path_lower: &str, want: &str) -> bool {
    let want = want.trim_start_matches('.').to_lowercase();
    !want.is_empty() && path_lower.ends_with(&format!(".{want}"))
}

/// The symbols lane's own kbcq/1 filter — `kind:`/`-kind:` over
/// V72-I1 — the `rails/1` facet atoms' POST-filter: `true` when no atom was
/// applied (nothing to narrow by, so everything passes, exactly like an
/// absent `path:`) and otherwise `true` only for a path the resolved noun
/// actually covers. A HARD gate, not the advisory `candidate_paths` hint —
/// a facet that left non-members in the page would not be a facet.
fn matches_rails(path: &str, selection: Option<&RailsSelection>) -> bool {
    match selection {
        Some(sel) if sel.applied => sel.paths.contains(path),
        _ => true,
    }
}

/// `Symbol::kind`, applied as a POST-filter beside `matches_filters`'s path
/// tests (the kind lives on the symbol, not the path, so it cannot ride the
/// same predicate).
fn matches_symbol_filters(hit: &super::SymbolHit, filters: &Filters) -> bool {
    if !filters.kind.is_empty()
        && !filters
            .kind
            .iter()
            .any(|k| hit.symbol.kind.eq_ignore_ascii_case(k))
    {
        return false;
    }
    if filters
        .not_kind
        .iter()
        .any(|k| hit.symbol.kind.eq_ignore_ascii_case(k))
    {
        return false;
    }
    true
}

/// `sort:` — re-order the PAGE a lane already selected (see
/// `grammar::SortKey`'s doc: this never changes which candidates the lane's
/// own ranking chose, only how the returned rows are presented). `None` and
/// `Some(Relevance)` both leave the lane's own order untouched.
fn apply_sort<T, F: Fn(&T) -> &str>(hits: &mut [T], sort: Option<SortKey>, path_of: F) {
    if sort == Some(SortKey::Path) {
        hits.sort_by(|a, b| path_of(a).cmp(path_of(b)));
    }
}

/// Over-fetch width for a `limit`-capped lane about to be `lang:`/`path:`
/// filtered — see the module doc.
fn oversample_limit(limit: usize) -> usize {
    limit.saturating_mul(OVERSAMPLE).clamp(limit, MAX_LIMIT)
}

/// `GET /api/search?q=&repo=&limit=`'s implementation — see the module doc
/// for the full contract. `is_loopback` is the caller's own pre-computed
/// loopback-ness (`routes::search_unified` derives it via
/// `kb_server::middleware::is_loopback_origin` before calling in, so this
/// fn stays free of any `axum`/request-shaped types and is directly
/// callable from tests).
pub async fn run(
    state: &SharedState,
    is_loopback: bool,
    raw_q: &str,
    repo_param: Option<&str>,
    limit: Option<usize>,
) -> UnifiedSearchResponse {
    let limit = routes::clamp_limit(limit);

    if raw_q.trim().is_empty() {
        let files_section = match routes::resolve_search_repos(state, repo_param) {
            Ok(repos) => {
                // 2026-08-31 incident (store.rs module doc): `Store` calls
                // reachable from async context must run on the blocking
                // pool, never inline on a tokio worker.
                let file_index = state.file_index.clone();
                let result = state
                    .store
                    .run_blocking(move |store| {
                        file_index.recent(store, &repos, limit, &LaneOpts::default())
                    })
                    .await;
                match result {
                    Ok(hits) => {
                        let truncated = hits.len() >= limit;
                        section(Lane::Files, to_value(&hits), truncated)
                    }
                    Err(e) => unavailable(Lane::Files, e.to_string()),
                }
            }
            Err(e) => unavailable(Lane::Files, e.message().to_string()),
        };
        return UnifiedSearchResponse {
            sections: vec![files_section],
            query_echo: raw_q.to_string(),
            // Nothing was parsed on this path (see the module doc's "Empty
            // query"), so there is no normalized form and nothing to warn
            // about — an empty string here is the honest answer, not a
            // placeholder.
            normalized: String::new(),
            diagnostics: Vec::new(),
            // Nothing was parsed, so nothing asked for facets. Staleness is
            // unconditional — a recents page is an index read like any
            // other.
            facets: None,
            stale: freshness(state),
        };
    }

    let parsed = grammar::parse(raw_q);
    let effective_repo = parsed
        .filters
        .repo
        .clone()
        .or_else(|| repo_param.map(str::to_string));
    let repos_result: Result<Vec<(String, i64)>, String> =
        routes::resolve_search_repos(state, effective_repo.as_deref())
            .map_err(|e| e.message().to_string());

    let want = |l: Lane| parsed.lanes.contains(&l);
    // `~~` is the ONLY way to select transcripts exclusively (see
    // `grammar`'s module doc) — an explicit, single-lane ask for a
    // loopback-only lane from a non-loopback caller gets an honest
    // `unavailable_reason` section (see the module doc's "Filters and repo
    // scoping" / V70-A3X); the "every lane" no-prefix case still drops
    // transcripts SILENTLY (absent, not `unavailable_reason`) for a
    // non-loopback caller — unchanged, since transcripts there is just one
    // of six ATTEMPTED lanes, not something the caller explicitly asked for.
    let transcripts_explicit = parsed.lanes == [Lane::Transcripts];
    let query = parsed.query.as_str();
    let filters = &parsed.filters;
    let text_mode = parsed.text_mode;
    let semantic_repo = effective_repo.as_deref();
    // V71-D1 — the per-signal flags come from config ONCE per request and
    // are handed to every lane, so two lanes can never disagree about
    // whether a factor is on.
    let factors = state.search_factors;
    let sort = parsed.sort;
    let explain = parsed.explain;
    // V71-D2 — `group:none` and an absent `group:` are different facts in
    // the grammar (a saved search can SAY ungrouped); both mean "no groups
    // on the wire" here, which is where the distinction stops mattering.
    let group = match parsed.group {
        Some(GroupKey::None) | None => None,
        Some(g) => Some(g),
    };
    let want_facets = parsed.facets;
    // V72-I1 — the `rails/1` facet atoms resolve ONCE per request, before
    // any lane runs, to a file set plus the caption that explains it. Every
    // narrowed lane then shares one answer: two lanes can never disagree
    // about what `model:Order` selected.
    let rails_sel = resolve_rails_selection(state, &repos_result, filters).await;
    let rails_sel = rails_sel.as_ref();

    let (files_out, symbols_out, text_out, semantic_out, sessions_out, transcripts_out) = tokio::join!(
        async {
            if !want(Lane::Files) {
                return None;
            }
            Some(
                run_files(
                    state,
                    &repos_result,
                    query,
                    filters,
                    limit,
                    factors,
                    sort,
                    explain,
                    rails_sel,
                )
                .await,
            )
        },
        async {
            if !want(Lane::Symbols) {
                return None;
            }
            Some(
                run_symbols(
                    state,
                    &repos_result,
                    query,
                    filters,
                    limit,
                    factors,
                    sort,
                    explain,
                    rails_sel,
                )
                .await,
            )
        },
        async {
            if !want(Lane::Text) {
                return None;
            }
            Some(
                run_text(
                    state,
                    &repos_result,
                    query,
                    filters,
                    text_mode,
                    limit,
                    factors,
                    sort,
                    rails_sel,
                )
                .await,
            )
        },
        async {
            if !want(Lane::Semantic) {
                return None;
            }
            Some(run_semantic(state, semantic_repo, query, limit).await)
        },
        async {
            if !want(Lane::Sessions) {
                return None;
            }
            Some(run_sessions(&state.kb_daemon, query, limit).await)
        },
        async {
            if !want(Lane::Transcripts) {
                return None;
            }
            if !is_loopback {
                return if transcripts_explicit {
                    Some(unavailable(
                        Lane::Transcripts,
                        "transcripts search is loopback-only",
                    ))
                } else {
                    None
                };
            }
            Some(run_transcripts(state, query, limit).await)
        },
    );

    let mut sections = Vec::with_capacity(LANE_ORDER.len());
    for s in [
        files_out,
        symbols_out,
        text_out,
        semantic_out,
        sessions_out,
        transcripts_out,
    ]
    .into_iter()
    .flatten()
    {
        sections.push(s);
    }

    // V71-D2 — both are computed over the sections EXACTLY as they are about
    // to be returned (post-limit, post-filter, post-truncate), which is what
    // makes `basis: "page"` true rather than aspirational.
    if let Some(by) = group {
        for sec in sections.iter_mut() {
            sec.groups = Some(results::group_hits(sec.lane, &sec.results, by));
        }
    }
    let facets = want_facets.then(|| {
        let counted: Vec<CountedSection<'_>> = sections
            .iter()
            .map(|s| CountedSection {
                lane: s.lane,
                results: &s.results,
            })
            .collect();
        results::facets_for(&counted)
    });

    UnifiedSearchResponse {
        sections,
        query_echo: raw_q.to_string(),
        normalized: parsed.normalized.clone(),
        diagnostics: parsed.diagnostics.clone(),
        facets,
        stale: freshness(state),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_files(
    state: &SharedState,
    repos: &Result<Vec<(String, i64)>, String>,
    query: &str,
    filters: &Filters,
    limit: usize,
    factors: Factors,
    sort: Option<SortKey>,
    explain: bool,
    rails: Option<&RailsSelection>,
) -> LaneSection {
    let repos = match repos {
        Ok(r) => r,
        Err(msg) => return unavailable(Lane::Files, msg.clone()),
    };
    let raw_limit = oversample_limit(limit);
    let q = query.trim().to_string();
    // One coarse `run_blocking` closure covers the store read AND the
    // (pure, CPU-only) filter/truncate pass — see store.rs's 2026-08-31
    // incident note: never round-trip the blocking pool per store call.
    let repos = repos.clone();
    let filters = filters.clone();
    let file_index = state.file_index.clone();
    let rails_owned = rails.cloned();
    let result = state
        .store
        .run_blocking(move |store| {
            // `path`/`repo:` are resolved as PRE-filters (V70-A3X, module
            // doc): `path_filter` narrows the candidate set INSIDE
            // `recent`/`search`, before their own internal ranking/
            // truncation, so a `path:`-matching hit can never be dropped by
            // the oversample-then-truncate dance below — `lang:` still
            // applies as the POST-filter it always was (`matches_filters`),
            // since it's not part of this fix's scope.
            let opts = LaneOpts {
                path_filter: filters.path.as_deref(),
                factors,
                explain,
                candidate_paths: None,
            };
            let hits = if q.is_empty() {
                file_index.recent(store, &repos, raw_limit, &opts)
            } else {
                let now_ms = chrono::Utc::now().timestamp_millis();
                file_index.search(store, &repos, &q, raw_limit, now_ms, &opts)
            };
            hits.map(|hits| {
                let raw_len = hits.len();
                let mut filtered: Vec<_> = hits
                    .into_iter()
                    .filter(|h| {
                        matches_filters(&h.path, &filters)
                            && matches_rails(&h.path, rails_owned.as_ref())
                    })
                    .collect();
                filtered.truncate(limit);
                apply_sort(&mut filtered, sort, |h| h.path.as_str());
                (filtered, raw_len)
            })
        })
        .await;
    match result {
        Ok((filtered, raw_len)) => {
            let mut s = section(Lane::Files, to_value(&filtered), raw_len >= raw_limit);
            if explain {
                s.explain = Some(lane_explain(&factors));
            }
            s.caption = rails.map(|r| r.caption());
            s
        }
        Err(e) => unavailable(Lane::Files, e.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_symbols(
    state: &SharedState,
    repos: &Result<Vec<(String, i64)>, String>,
    query: &str,
    filters: &Filters,
    limit: usize,
    factors: Factors,
    sort: Option<SortKey>,
    explain: bool,
    rails: Option<&RailsSelection>,
) -> LaneSection {
    let repos = match repos {
        Ok(r) => r,
        Err(msg) => return unavailable(Lane::Symbols, msg.clone()),
    };
    let q = query.trim();
    if q.is_empty() {
        return unavailable(Lane::Symbols, "q must not be empty");
    }
    let raw_limit = oversample_limit(limit);
    let repos = repos.clone();
    let q = q.to_string();
    let filters = filters.clone();
    let symbol_index = state.symbol_index.clone();
    let rails_owned = rails.cloned();
    let result = state
        .store
        .run_blocking(move |store| {
            // See `run_files`'s equivalent comment — `path`/`repo:` are
            // PRE-filters here too (V70-A3X).
            let opts = LaneOpts {
                path_filter: filters.path.as_deref(),
                factors,
                explain,
                candidate_paths: None,
            };
            symbol_index
                .search(store, &repos, &q, raw_limit, &opts)
                .map(|hits| {
                    let raw_len = hits.len();
                    let mut filtered: Vec<_> = hits
                        .into_iter()
                        .filter(|h| {
                            matches_filters(&h.path, &filters)
                                && matches_symbol_filters(h, &filters)
                                && matches_rails(&h.path, rails_owned.as_ref())
                        })
                        .collect();
                    filtered.truncate(limit);
                    apply_sort(&mut filtered, sort, |h| h.path.as_str());
                    (filtered, raw_len)
                })
        })
        .await;
    match result {
        Ok((filtered, raw_len)) => {
            let mut s = section(Lane::Symbols, to_value(&filtered), raw_len >= raw_limit);
            if explain {
                s.explain = Some(lane_explain(&factors));
            }
            s.caption = rails.map(|r| r.caption());
            s
        }
        Err(e) => unavailable(Lane::Symbols, e.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_text(
    state: &SharedState,
    repos: &Result<Vec<(String, i64)>, String>,
    query: &str,
    filters: &Filters,
    text_mode: TextMode,
    limit: usize,
    factors: Factors,
    sort: Option<SortKey>,
    rails: Option<&RailsSelection>,
) -> LaneSection {
    let repos = match repos {
        Ok(r) => r,
        Err(msg) => return unavailable(Lane::Text, msg.clone()),
    };
    let (repo_name, repo_id) = match repos.len() {
        0 => return unavailable(Lane::Text, "no repos configured"),
        1 => repos[0].clone(),
        _ => {
            return unavailable(
                Lane::Text,
                "text search needs exactly one repo — pass ?repo= or a repo:<name> filter \
                 (multiple repos are configured)",
            )
        }
    };
    let q = query.trim();
    if q.is_empty() {
        return unavailable(Lane::Text, "q must not be empty");
    }
    let Some(repo_entry) = state.repos.iter().find(|r| r.name == repo_name) else {
        return unavailable(Lane::Text, format!("no such repo: {repo_name:?}"));
    };
    let regex = matches!(text_mode, TextMode::Regex);
    let case_sensitive = filters.case.unwrap_or(false);
    // `search_text` starts with a store read (`list_files`) and continues
    // straight into the (also blocking) file-content scan — one closure
    // covers the whole contiguous blocking computation.
    let repo_root = repo_entry.path.clone();
    let q = q.to_string();
    let filters = filters.clone();
    // V71-D1b — the symbol-name candidate signal for `search_text`'s
    // candidate-first reorder (that fn's module doc, "Scan order"). Cloning
    // the `Arc<SymbolIndex>` here (not the query) is what lets the closure
    // below call `cached_snapshot_if_warm` — a non-rebuilding peek — rather
    // than reaching for `state.store` a second time outside `run_blocking`.
    let symbol_index = state.symbol_index.clone();
    let rails_owned = rails.cloned();
    let result = state
        .store
        .run_blocking(move |store| {
            // `path`/`repo:` are PRE-filters here too (V70-A3X) — passed
            // straight into `search_text`, which skips a non-matching file
            // before it's ever opened, keeping the 300ms budget for files
            // that actually matter (see that fn's doc).
            let atoms = super::matcher::identifier_atoms(&q);
            let candidate_paths =
                super::symbol_candidate_paths(&symbol_index, store, repo_id, &atoms);
            let opts = LaneOpts {
                path_filter: filters.path.as_deref(),
                factors,
                explain: false,
                candidate_paths: candidate_paths.as_ref(),
            };
            super::search_text(
                store,
                &repo_root,
                repo_id,
                &q,
                regex,
                case_sensitive,
                super::text::DEFAULT_TIME_BUDGET,
                &opts,
            )
            .map(|mut resp| {
                resp.results.retain(|r| {
                    matches_filters(&r.path, &filters)
                        && matches_rails(&r.path, rails_owned.as_ref())
                });
                // `search_text` itself has no per-request result-count
                // limit (only its own fixed MAX_TOTAL_MATCHES/
                // MAX_MATCHES_PER_FILE caps — see that module's doc), so
                // the box's own `limit` applies here as a FILE-count cap,
                // same "cut further, note it as truncated" shape the
                // files/symbols lanes use.
                let file_count = resp.results.len();
                resp.results.truncate(limit);
                apply_sort(&mut resp.results, sort, |r| r.path.as_str());
                let truncated = resp.truncated || resp.time_budget_exceeded || file_count > limit;
                (resp.results, truncated)
            })
        })
        .await;
    match result {
        Ok((results, truncated)) => {
            let mut s = section(Lane::Text, to_value(&results), truncated);
            s.caption = rails.map(|r| r.caption());
            s
        }
        Err(e) => unavailable(Lane::Text, e.to_string()),
    }
}

/// V72-I1 — resolve the query's `rails/1` facet atoms against the ONE repo
/// in scope. `None` when the query carries no atom at all (the ordinary
/// case; nothing is read and nothing is narrowed).
///
/// Refuses rather than guesses on two inputs: an unresolvable repo set and
/// a scope holding more than one repo. Two repos' file paths are both
/// repo-relative, and the lanes' post-filter sees only a path — so a
/// `model:Order` resolved in repo A would silently keep repo B's
/// same-named file. The atom is left UNAPPLIED with that stated as the
/// caption instead (`kbc-scope/1`'s posture: a scope that will not resolve
/// is not applied, and the caller is told).
async fn resolve_rails_selection(
    state: &SharedState,
    repos: &Result<Vec<(String, i64)>, String>,
    filters: &Filters,
) -> Option<RailsSelection> {
    // `filters.model` / `filters.controller` / `filters.action` /
    // `filters.route` / `filters.job` / `filters.rails` — the six
    // `FILTER_SPECS` consumer expressions, read through the ONE accessor
    // that keeps their order the grammar's.
    let atoms = filters.rails_atoms();
    if atoms.is_empty() {
        return None;
    }
    let nouns: Vec<&'static str> = atoms.iter().map(|(n, _)| *n).collect();
    let refused = |reason: String| {
        Some(RailsSelection {
            nouns: nouns.clone(),
            applied: false,
            reason: Some(reason),
            paths: std::collections::HashSet::new(),
        })
    };
    let repos = match repos {
        Ok(r) => r,
        Err(msg) => return refused(msg.clone()),
    };
    let (repo_name, repo_id) = match repos.len() {
        1 => repos[0].clone(),
        n => {
            return refused(format!(
                "a Rails noun filter needs exactly one repo in scope ({n} are) — pass ?repo=                  or a repo:<name> filter"
            ))
        }
    };
    let Some(entry) = state.repos.iter().find(|r| r.name == repo_name) else {
        return refused(format!("no such repo: {repo_name:?}"));
    };
    let repo_root = entry.path.clone();
    Some(
        state
            .store
            .run_blocking(move |store| {
                crate::rails::filter::resolve(store, &repo_root, repo_id, &atoms)
            })
            .await,
    )
}

async fn run_semantic(
    state: &SharedState,
    repo_filter: Option<&str>,
    query: &str,
    limit: usize,
) -> LaneSection {
    let q = query.trim();
    if q.is_empty() {
        return unavailable(Lane::Semantic, "q must not be empty");
    }
    if let Some(name) = repo_filter {
        if !state.repos.iter().any(|r| r.name == name) {
            return unavailable(Lane::Semantic, format!("no such repo: {name:?}"));
        }
        if !state.semantic.repo_enabled(name) {
            return unavailable(
                Lane::Semantic,
                format!(
                    "semantic search is not enabled for repo {name:?} — add it to \
                     [semantic] repos (with [semantic] enabled = true) in kb-code.toml"
                ),
            );
        }
    } else if !state.semantic.enabled || state.semantic.repos.is_empty() {
        return unavailable(
            Lane::Semantic,
            "semantic search is disabled — set [semantic] enabled = true and list at least \
             one repo under [semantic] repos in kb-code.toml",
        );
    }
    let (Some(chunk_store), Some(embedder)) = (
        state.semantic_chunk_store.clone(),
        state.semantic_embedder.clone(),
    ) else {
        return unavailable(
            Lane::Semantic,
            "semantic search is disabled for this daemon",
        );
    };

    let repo_filter_owned = repo_filter.map(str::to_string);
    let limit_u32 = limit as u32;
    let fut = async move {
        let (q, limit) = semantic::search::validate_query(q, Some(limit_u32))?;
        let vec = semantic::search::embed_query(&embedder, &q).await?;
        semantic::search::search(&chunk_store, &vec, repo_filter_owned.as_deref(), limit).await
    };
    match tokio::time::timeout(SEMANTIC_STAGE_BUDGET, fut).await {
        Ok(Ok(hits)) => section(Lane::Semantic, to_value(&hits), false),
        Ok(Err(e)) => unavailable(Lane::Semantic, e.to_string()),
        // Missed the staged budget — hand back `pending: true` and abandon
        // the in-flight future (see the module doc's "Semantic staging").
        Err(_) => pending_section(Lane::Semantic),
    }
}

async fn run_sessions(cfg: &KbDaemonSection, query: &str, limit: usize) -> LaneSection {
    let q = query.trim();
    if q.is_empty() {
        return unavailable(Lane::Sessions, "q must not be empty");
    }
    match sessions::search(cfg, q, limit).await {
        Ok(hits) => section(Lane::Sessions, to_value(&hits), false),
        Err(e) => unavailable(Lane::Sessions, e.to_string()),
    }
}

async fn run_transcripts(state: &SharedState, query: &str, limit: usize) -> LaneSection {
    let q = query.trim();
    if q.is_empty() {
        return unavailable(Lane::Transcripts, "q must not be empty");
    }
    let limit = limit.clamp(1, transcripts::search::MAX_LIMIT);
    let q_owned = q.to_string();
    let transcripts_root = state.transcripts_root.clone();
    // `to_hits` re-reads the source JSONL for each snippet — also blocking
    // I/O, folded into the same closure as the FTS5 store read.
    let result = state
        .store
        .run_blocking(move |store| {
            store
                .search_transcripts(&q_owned, limit, None, None)
                .map(|rows| transcripts::search::to_hits(&transcripts_root, rows, &q_owned))
        })
        .await;
    match result {
        Ok(hits) => section(Lane::Transcripts, to_value(&hits), false),
        Err(e) => {
            let msg = if transcripts::search::is_invalid_query_error(&e) {
                transcripts::search::invalid_query_message(&e)
            } else {
                e.to_string()
            };
            unavailable(Lane::Transcripts, msg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_filters_with_no_filters_passes_everything() {
        assert!(matches_filters("src/lib.rs", &Filters::default()));
    }

    #[test]
    fn matches_filters_path_is_case_insensitive_substring() {
        let f = Filters {
            path: Some("SRC/".to_string()),
            ..Filters::default()
        };
        assert!(matches_filters("src/lib.rs", &f));
        assert!(!matches_filters("tests/lib.rs", &f));
    }

    #[test]
    fn matches_filters_lang_matches_detected_language_case_insensitively() {
        let f = Filters {
            lang: Some("RUST".to_string()),
            ..Filters::default()
        };
        assert!(matches_filters("src/lib.rs", &f));
        assert!(!matches_filters("src/app.py", &f));
        // No grammar for this extension at all — never matches a lang
        // filter, regardless of casing/spelling.
        assert!(!matches_filters("README.md", &f));
    }

    #[test]
    fn matches_filters_lang_and_path_both_apply() {
        let f = Filters {
            lang: Some("python".to_string()),
            path: Some("app".to_string()),
            ..Filters::default()
        };
        assert!(matches_filters("pkg/app.py", &f));
        assert!(!matches_filters("pkg/other.py", &f));
        assert!(!matches_filters("pkg/app.rs", &f));
    }

    #[test]
    fn oversample_limit_widens_but_never_exceeds_the_lane_ceiling() {
        assert_eq!(oversample_limit(10), 40);
        assert_eq!(oversample_limit(MAX_LIMIT), MAX_LIMIT);
        // Never narrower than the caller's own limit.
        assert!(oversample_limit(1) >= 1);
    }

    #[test]
    fn lane_section_omits_unavailable_reason_and_pending_when_absent() {
        let s = section(Lane::Files, serde_json::json!([]), false);
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("unavailable_reason").is_none());
        assert!(v.get("pending").is_none());
        assert_eq!(v["lane"], "files");
    }

    #[test]
    fn unavailable_section_carries_the_reason_and_empty_results() {
        let s = unavailable(Lane::Symbols, "q must not be empty");
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["unavailable_reason"], "q must not be empty");
        assert_eq!(v["results"], serde_json::json!([]));
        assert!(v.get("pending").is_none());
    }

    #[test]
    fn pending_section_sets_pending_true_with_empty_results() {
        let s = pending_section(Lane::Semantic);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["pending"], true);
        assert_eq!(v["results"], serde_json::json!([]));
        assert!(v.get("unavailable_reason").is_none());
    }
}
