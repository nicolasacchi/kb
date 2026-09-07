//! B2 — `GET /api/resolve?repo=&path=&line=&col=[&ref=]`: given a 1-based
//! line + 0-based col (tree-sitter's own `Point` convention, matching every
//! other position field this crate serves), answer "what is this
//! identifier, and where else might it be defined." Deliberately
//! TAGS-TIER + occurrence-tier (plus an OPT-IN exact tier, S1 — see below),
//! never real scope/type resolution — same honest-labeling convention
//! `agentview::xref`'s `defs`/`xrefs` already carry in their own JSON
//! (`approximate`/`note`); this endpoint's `note` field says the same thing
//! in its own words (see [`RESOLVE_NOTE`]).
//!
//! # Locating the identifier at the position
//!
//! When the file's blob has occurrence rows (`store::Store::
//! has_occurrences` — one of `lang::TOKEN_LEVEL_LANG_IDS`'s languages, OR
//! ANY language a `scip ingest` has covered — `has_occurrences` doesn't
//! distinguish source), [`store::Store::occurrence_at`] looks up the exact
//! occurrence covering `(line, col)`. Two sub-cases:
//! - a hit → `ident`/`role` come straight from that occurrence. S1: when a
//!   `'ts'` row and a `'scip'` row both cover the position, the `scip` one
//!   wins (`occurrence_at`'s own tie-break) — this is the whole of "scip
//!   rows also make the position lookup itself exact," no separate code
//!   path here.
//! - a miss (the position doesn't land on any indexed identifier, e.g.
//!   whitespace or punctuation) → falls through to the same word-scan
//!   below, with `role: None` (we know a word is there, but not its
//!   grammar-derived role).
//!
//! When the blob has NO occurrence rows at all (an old blob predating this
//! pass, or a language `occurrences.rs` doesn't cover and no `scip ingest`
//! has ever touched), [`word_at`] extracts the `[A-Za-z0-9_]` run touching
//! `col` directly from the file's text — a plain word-boundary scan, no
//! parsing.
//!
//! # Candidate ranking
//!
//! 1. **`"scip-exact"`** (S1) — every `source = 'scip'`, `role = "def"`
//!    occurrence named `ident` in the SAME blob
//!    ([`store::Store::scip_def_occurrences_by_name`]), enriched from
//!    `symbols` the same way lower tiers are. Empty (and thus invisible)
//!    for any repo that has never had a `.scip` index ingested — this tier
//!    is entirely opt-in. Ranked FIRST: a SCIP index comes from a real
//!    language server/compiler front-end. Trust class: **`exact`**.
//! 2. **`"locals"`** (V3.G1) — when the position's occurrence row carries a
//!    non-NULL `local_def_ordinal` (filled by the lexical scope graph in
//!    `crate::locals` for the four proof languages), resolve to THAT
//!    definition occurrence. Scope-proven same-file binding. Trust class:
//!    **`exact`**. Ranked ABOVE the non-scope-proven file-local tier.
//! 3. **`"file-local"`** — every `source = 'ts'`, `role = "def"` occurrence
//!    named `ident` in the SAME file/blob
//!    ([`store::Store::def_occurrences_by_name`]), optionally enriched with
//!    `kind`/`container`/`signature`/`doc` when the same `(name, line)` also
//!    has a `symbols` row. Name match only — NOT scope-proven. Trust class:
//!    **`likely`**.
//! 4. **`"import-heuristic"`** (B4) — ONLY attempted when file-local found
//!    nothing (see the cost note below): if `ident` (or its de-aliased
//!    original name) is imported in the CURRENT file, [`crate::imports`]
//!    follows the import's own module path to the file it names. Trust
//!    class: **`likely`**.
//! 5. **`"framework-convention"`** (PRR-N5) — when the file is Rails-lens-
//!    eligible (`frameworks::rails_lens_relevant_path`) and the REQUESTED
//!    POSITION (not `ident` — see the tier's own comment below for why this
//!    check is position-gated rather than name-gated) lands on a recognized
//!    DSL construct — i.e. a `rails_edges` row whose `src_path`/`src_line`
//!    matches this request — jump straight to that row's `dst_path`
//!    (file-level; line defaults to 1, since `rails_edges` carries no
//!    `dst_line` column). Trust class mirrors the row's OWN `Trust`
//!    (`likely` or `candidate`) — NEVER `exact` (this lens's own
//!    oracle-bar posture; see `frameworks`'s module doc). Ranked after
//!    import-heuristic, before tags-approx: a recognized framework
//!    convention is more specific than a bare same-repo name grep.
//! 6. **`"tags-approx"`**, same repo — every `symbols` row named `ident`
//!    anywhere in the CURRENT repo, SAME-CONTAINER-first. Trust class:
//!    **`candidate`**.
//! 7. **`"tags-approx"`**, other repos — every OTHER configured repo's
//!    `symbols` matches, in `AppState.repos`' own configured order. Trust
//!    class: **`candidate`**.
//!
//! Each candidate also carries an additive **`class`** field
//! (`"exact"` | `"likely"` | `"candidate"`) — the trust contract. The
//! existing **`precision`** field is the provenance/`via` tier (unchanged
//! wire name). A wrong `exact`-class result is a release blocker; when
//! unsure, classify DOWN.
//!
//! A candidate identical to one already emitted at a higher-priority tier
//! (same `repo`/`path`/`line`) is dropped — the SAME location never appears
//! twice. [`MAX_CANDIDATES`] bounds the response; `total` is the post-cap
//! count actually returned (this Wave has no `?limit=`/pagination surface).
//!
//! Cost note (B4): the import-heuristic tier costs one extra tree-sitter
//! parse of the CURRENT file (`crate::imports::import_origin`) — paid ONLY
//! when file-local found zero candidates for `ident`. Tier 1 (`scip-exact`)
//! and tier 2 (`locals`) are indexed lookups — no measurable extra cost.

use crate::config::RepoEntry;
use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const RESOLVE_SCHEMA: &str = "resolve/1";
/// PRR-L2 — the lip/1 live-LSP overlay's TOP tier (design-lip.md's
/// "kb-code-server integration": "a new TOP tier `lsp-live`... consulted
/// FIRST when (repo, file-lang) has a configured provider"). Computed at
/// the async HTTP-handler layer (`crate::lip::lsp_live_definitions`,
/// spliced in by `resolve_route` AFTER this module's own synchronous
/// `resolve_position` returns — see that fn's doc for why) rather than
/// inside `resolve_position` itself; still ranked ahead of `scip-exact`
/// once merged. Minted `exact` ONLY when the adapter's blob guard passed
/// AND kb-code's own re-check confirms the response's `verified_blob_sha`
/// against a FRESH read of the file (`crate::lip::verify_blob_freshness`)
/// — any mismatch discards the answer entirely, never downgrades it.
pub const PRECISION_LSP_LIVE: &str = "lsp-live";
/// S1 — see the module doc's tier 1. The ONE precision tier backed by a
/// real language server/compiler front-end rather than this crate's own
/// tree-sitter heuristics.
pub const PRECISION_SCIP_EXACT: &str = "scip-exact";
/// V3.G1 — scope-proven same-file binding via `crate::locals`.
pub const PRECISION_LOCALS: &str = "locals";
pub const PRECISION_FILE_LOCAL: &str = "file-local";
/// B4 — see the module doc's import-heuristic tier.
pub const PRECISION_IMPORT_HEURISTIC: &str = "import-heuristic";
/// PRR-N5 — see the module doc's tier 4.5 (Rails-lens convention match).
pub const PRECISION_FRAMEWORK: &str = "framework-convention";
pub const PRECISION_TAGS_APPROX: &str = "tags-approx";

/// Trust class — the contract field on every candidate. Independent of
/// [`Candidate::precision`] (provenance/`via`).
pub const CLASS_EXACT: &str = "exact";
pub const CLASS_LIKELY: &str = "likely";
pub const CLASS_CANDIDATE: &str = "candidate";

/// Map a provenance/`via` precision tier to its trust class.
pub fn class_for_precision(precision: &str) -> &'static str {
    match precision {
        PRECISION_LSP_LIVE
        | PRECISION_SCIP_EXACT
        | PRECISION_LOCALS
        // V71-E1 — Ruby's read-time locals lane under D4's STRICT rule
        // (`crate::usages2`); a distinct tier name so an `exact` says WHICH
        // rule minted it, but the same class.
        | crate::usages2::PRECISION_RUBY_STRICT => CLASS_EXACT,
        PRECISION_FILE_LOCAL
        | PRECISION_IMPORT_HEURISTIC
        | crate::intel::cross_file::PRECISION_IMPORT_FILTERED => CLASS_LIKELY,
        // PRR-N5 — framework-convention candidates are always pushed via
        // `candidate_with_class` with the edge's OWN `Trust` (Likely or
        // Candidate) explicitly threaded in (see the resolve tier below);
        // this arm only matters if this fn is ever called generically for
        // that precision string, and classifies DOWN per this fn's own
        // policy (never assume Likely for a row this fn never actually saw).
        PRECISION_TAGS_APPROX | PRECISION_FRAMEWORK => CLASS_CANDIDATE,
        // Unknown tiers classify DOWN.
        _ => CLASS_CANDIDATE,
    }
}

/// Carried verbatim in every response — see the module doc's opening
/// paragraph; mirrors `agentview::xref::REFS_NOTE`'s honesty convention.
pub const RESOLVE_NOTE: &str = "tiered match (OPT-IN scip-exact + scope-proven locals exact, \
     then likely file-local/import-heuristic/import-filtered, then candidate tags-approx) — \
     exact stays reserved for scip/locals in v3.0 (a unique import-filtered hit is only \
     likely, by design); below exact, a same-named binding in an unrelated scope, an import \
     followed by path alone with no type inference, or an unrelated shadowing definition, \
     can appear as a candidate. Cross-file name matches are filtered by the import graph \
     (same-dir + direct import edges); if the filter empties a non-empty list the unfiltered \
     list returns as class=candidate (fuzzy fallback). Call-site arity demotes, never drops.";
/// Response cap — this Wave has no `?limit=`/pagination query param (see
/// the module doc).
pub const MAX_CANDIDATES: usize = 50;

#[derive(Debug, Deserialize)]
pub struct ResolveParams {
    pub repo: String,
    pub path: String,
    /// 1-based.
    pub line: u32,
    /// 0-based byte offset within `line` — tree-sitter's own convention,
    /// matching every other position field this crate serves.
    pub col: u32,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Position {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub kind: Option<String>,
    pub container: Option<String>,
    pub signature: Option<String>,
    pub doc: Option<String>,
    /// Provenance/`via` tier — see module doc (`scip-exact`/`locals`/
    /// `file-local`/`import-heuristic`/`tags-approx`).
    pub precision: &'static str,
    /// Trust class — `"exact"` | `"likely"` | `"candidate"`. Additive
    /// (V3.G1); independent of [`Self::precision`].
    pub class: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolveOut {
    pub schema: &'static str,
    pub ident: String,
    pub position: Position,
    /// `"def"` | `"ref"` | `"import"` when resolved via the occurrences
    /// table; `None` when resolved via the plain word-scan fallback (we
    /// know a word is there, but not its grammar-derived role).
    pub role: Option<String>,
    pub candidates: Vec<Candidate>,
    pub total: usize,
    pub note: &'static str,
}

/// `GET /api/resolve?repo=&path=&line=&col=[&ref=]`.
///
/// PRR-L2: after the synchronous ladder (`resolve_position`, unchanged)
/// resolves its own candidates, this handler ALSO consults `crate::lip`'s
/// lsp-live overlay and splices any answer in as the TOP tier
/// (`crate::lip::prepend_candidates`) — see `crate::lip`'s module doc for
/// why the async HTTP call lives here rather than inside
/// `resolve_position` (which stays pure/sync/directly-unit-testable, same
/// as before this Wave). On refusal/timeout/absence/no-configured-provider
/// the existing ladder's own response is returned byte-for-byte unchanged.
pub async fn resolve_route(
    State(state): State<SharedState>,
    Query(params): Query<ResolveParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();
    let path = params.path.clone();
    let line = params.line;
    let col = params.col;
    let rev = params.rev.clone();
    // 2026-08-31 incident (store.rs module doc): the whole sync
    // occurrence/symbol/import ladder runs on the blocking pool in one
    // hop; the lsp-live overlay below is the async leg and stays outside.
    let state_bg = state.clone();
    let repo_bg = repo.clone();
    let mut out = state
        .store
        .run_blocking(move |store| {
            resolve_position(
                store,
                &state_bg.repos,
                &state_bg.repo_ids,
                &repo_bg,
                repo_id,
                &path,
                line,
                col,
                rev.as_deref(),
            )
        })
        .await?;
    let lsp_live = crate::lip::lsp_live_definitions(
        &state,
        &repo,
        &params.path,
        params.rev.as_deref(),
        params.line,
        params.col,
    )
    .await;
    crate::lip::prepend_candidates(&mut out, lsp_live);
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `pub(crate)` — narrow deps (no `SharedState`) so this is directly
/// unit-testable against a fixture store + a real temp working tree, no
/// daemon boot, same precedent as `agentview::xref::resolve_defs`/
/// `resolve_refs`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_position(
    store: &Store,
    repos: &[RepoEntry],
    repo_ids: &HashMap<String, i64>,
    repo: &RepoEntry,
    repo_id: i64,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
) -> Result<ResolveOut, ApiError> {
    if line < 1 {
        return Err(ApiError::bad_request("line must be >= 1 (1-based)"));
    }
    let read = read_repo_file(repo, path, rev)?;
    let content = std::str::from_utf8(&read.bytes).map_err(|_| {
        ApiError::bad_request(format!(
            "{path}: not valid UTF-8 — position resolve needs text content"
        ))
    })?;
    let lines: Vec<&str> = content.split('\n').collect();
    let line_idx = (line - 1) as usize;
    let Some(line_text) = lines.get(line_idx) else {
        return Err(ApiError::bad_request(format!(
            "line {line} is out of range — {path} has {} line(s)",
            lines.len()
        )));
    };
    // Tolerate a trailing `\r` (CRLF line endings) without counting it as
    // part of the line's own byte range.
    let line_bytes = line_text.strip_suffix('\r').unwrap_or(line_text).as_bytes();
    if col as usize > line_bytes.len() {
        return Err(ApiError::bad_request(format!(
            "col {col} is out of range for line {line} ({} bytes)",
            line_bytes.len()
        )));
    }

    let lang_info = crate::lang::detect(path, Some(&read.bytes));
    let salt = lang_info.map(|l| l.symbol_salt);

    let has_occurrences = match salt {
        Some(salt) => store.has_occurrences(&read.blob_hash, salt)?,
        None => false,
    };

    // Keep the full occurrence row when present — the locals arm needs
    // `local_def_ordinal` from it.
    let hit_occurrence = if has_occurrences {
        let salt = salt.unwrap();
        store.occurrence_at(&read.blob_hash, salt, line, col)?
    } else {
        None
    };

    let (ident, role) = if let Some(ref o) = hit_occurrence {
        (o.name.clone(), Some(o.role.clone()))
    } else {
        let word = word_at(line_bytes, col as usize).ok_or_else(|| {
            ApiError::bad_request(format!("no identifier at {path}:{line}:{col}"))
        })?;
        (word, None)
    };

    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen: HashSet<(String, String, u32)> = HashSet::new();

    // Computed once, shared by scip-exact / locals / file-local enrichment
    // — all look up the SAME blob's `symbols` rows by `(name, line range)`.
    let blob_symbols = match salt {
        Some(salt) => store.symbols_for_blob(&read.blob_hash, salt)?,
        None => Vec::new(),
    };
    let enrich_from_symbols = |occ_line: u32| {
        blob_symbols
            .iter()
            .find(|s| s.name == ident && s.line_start <= occ_line && occ_line <= s.line_end)
    };

    // (1) scip-exact (S1) — every `source = 'scip'` def occurrence named
    // `ident` in THIS blob, ranked FIRST (see the module doc's tier 1).
    // Empty for any repo that has never had a `.scip` index ingested.
    if let Some(salt) = salt {
        for occ in store.scip_def_occurrences_by_name(&read.blob_hash, salt, &ident)? {
            let enrich = enrich_from_symbols(occ.line);
            push_candidate(
                &mut candidates,
                &mut seen,
                candidate(
                    repo.name.clone(),
                    path.to_string(),
                    occ.line,
                    enrich.map(|s| s.kind.clone()),
                    enrich.and_then(|s| s.container.clone()),
                    enrich.and_then(|s| s.signature.clone()),
                    enrich.and_then(|s| s.doc.clone()),
                    PRECISION_SCIP_EXACT,
                ),
            );
        }
    }

    // (2) locals (V3.G1) — scope-proven same-file binding via
    // `local_def_ordinal` on the position's occurrence row.
    if let (Some(salt), Some(hit)) = (salt, hit_occurrence.as_ref()) {
        if let Some(def_ord) = hit.local_def_ordinal {
            if let Some(def_occ) = store.occurrence_by_ordinal(&read.blob_hash, salt, def_ord)? {
                let enrich = enrich_from_symbols(def_occ.line);
                push_candidate(
                    &mut candidates,
                    &mut seen,
                    candidate(
                        repo.name.clone(),
                        path.to_string(),
                        def_occ.line,
                        enrich.map(|s| s.kind.clone()),
                        enrich.and_then(|s| s.container.clone()),
                        enrich.and_then(|s| s.signature.clone()),
                        enrich.and_then(|s| s.doc.clone()),
                        PRECISION_LOCALS,
                    ),
                );
            }
        }
    }

    // (3) file-local: every `source = 'ts'` def-role occurrence named
    // `ident` in THIS blob. Tracked separately from `candidates.is_empty()`
    // — a scip/locals hit must NOT suppress the B4 cost-gated
    // import-heuristic tier's own "did FILE-LOCAL find anything" check.
    let mut file_local_found_any = false;
    if let Some(salt) = salt {
        for occ in store.def_occurrences_by_name(&read.blob_hash, salt, &ident)? {
            file_local_found_any = true;
            let enrich = enrich_from_symbols(occ.line);
            push_candidate(
                &mut candidates,
                &mut seen,
                candidate(
                    repo.name.clone(),
                    path.to_string(),
                    occ.line,
                    enrich.map(|s| s.kind.clone()),
                    enrich.and_then(|s| s.container.clone()),
                    enrich.and_then(|s| s.signature.clone()),
                    enrich.and_then(|s| s.doc.clone()),
                    PRECISION_FILE_LOCAL,
                ),
            );
        }
    }

    // (4) import-heuristic (B4) — only attempted when file-local found
    // nothing for `ident` (see the module doc's cost note).
    if !file_local_found_any {
        if let Some(lang_info) = lang_info {
            if crate::imports::supports(lang_info.id) {
                if let Some(origin) =
                    crate::imports::import_origin(lang_info.id, &read.bytes, &ident)
                {
                    if let Some(target_rel) = crate::imports::resolve_module_file(
                        &repo.path,
                        std::path::Path::new(path),
                        lang_info.id,
                        &origin.raw_module,
                    ) {
                        if let Some(target_rel_str) = target_rel.to_str() {
                            if let Some(file_row) = store.get_file(repo_id, target_rel_str)? {
                                if let Some(target_lang) = crate::lang::for_id(&file_row.lang) {
                                    let search_name =
                                        origin.alias_of.as_deref().unwrap_or(ident.as_str());
                                    let mut hits: Vec<Candidate> = store
                                        .symbols_for_blob(
                                            &file_row.blob_hash,
                                            target_lang.symbol_salt,
                                        )?
                                        .into_iter()
                                        .filter(|s| s.name == search_name)
                                        .map(|s| {
                                            candidate(
                                                repo.name.clone(),
                                                target_rel_str.to_string(),
                                                s.line_start,
                                                Some(s.kind),
                                                s.container,
                                                s.signature,
                                                s.doc,
                                                PRECISION_IMPORT_HEURISTIC,
                                            )
                                        })
                                        .collect();
                                    hits.sort_by_key(|c| c.line);
                                    for c in hits {
                                        push_candidate(&mut candidates, &mut seen, c);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // (4.5) framework-convention (PRR-N5) — POSITION-gated, not name-gated:
    // the clicked `ident` may be a bare word inside a string-literal DSL
    // argument (e.g. "row" out of `render partial: 'row'`), and ERB/Ruby
    // blobs may carry no occurrence rows at all for that literal — so this
    // tier never looks at `ident`. Any `rails_edges` row whose `src_path`/
    // `src_line` matches THIS request's position is a recognized DSL
    // construct (see `frameworks::rails_lens_relevant_path`'s own doc for
    // the cheap path-pattern pre-check — a non-Rails repo or an irrelevant
    // path never even queries the store).
    if crate::frameworks::rails_lens_relevant_path(path) {
        for edge in store.rails_edges_by_src_path(repo_id, path)? {
            if edge.src_line != Some(line) {
                continue;
            }
            let Some(dst_path) = edge.dst_path.clone() else {
                continue; // no file target (e.g. a turbo_stream dom_id edge) — nothing to jump to.
            };
            let class = match edge.trust {
                crate::frameworks::Trust::Likely => CLASS_LIKELY,
                crate::frameworks::Trust::Candidate => CLASS_CANDIDATE,
            };
            push_candidate(
                &mut candidates,
                &mut seen,
                candidate_with_class(
                    repo.name.clone(),
                    dst_path,
                    // rails_edges carries no dst LINE (V0026's schema) — a
                    // file-level jump is honest, never a fabricated line.
                    1,
                    edge.dst_kind.clone(),
                    edge.dst_symbol.clone(),
                    None,
                    None,
                    PRECISION_FRAMEWORK,
                    class,
                ),
            );
        }
    }

    // The nearest enclosing symbol's own container at the query position —
    // used to rank same-repo tags below (see the module doc).
    let repo_symbols = store.symbols_for_repo(repo_id)?;
    let current_container = repo_symbols
        .iter()
        .filter(|(p, s)| p == path && s.line_start <= line && line <= s.line_end)
        .min_by_key(|(_, s)| s.line_end.saturating_sub(s.line_start))
        .and_then(|(_, s)| s.container.clone());

    // V3.G2 — import-reachability set for the querying file (direct edges).
    // Lazy repair: if this file has import_specs but zero edges (target
    // files landed after the first pass), re-resolve once against the
    // live files table so cross-file filtering is not stuck empty.
    let import_targets: HashSet<String> = match store.file_id(repo_id, path)? {
        Some(fid) => {
            let mut targets: HashSet<String> =
                store.import_target_paths(fid)?.into_iter().collect();
            if targets.is_empty() {
                if let (Some(li), Some(root)) = (lang_info, store.repo_root(repo_id)?) {
                    if crate::imports::supports(li.id) {
                        let specs = store.import_specs_for_blob(&read.blob_hash, li.symbol_salt)?;
                        if !specs.is_empty() {
                            let resolved = crate::import_graph::resolve_import_edges(
                                std::path::Path::new(&root),
                                std::path::Path::new(path),
                                li.id,
                                &specs,
                            );
                            let mut edges: Vec<(String, i64)> = Vec::new();
                            for (raw_spec, target_rel) in resolved {
                                if let Some(tid) = store.file_id(repo_id, &target_rel)? {
                                    edges.push((raw_spec, tid));
                                }
                            }
                            if !edges.is_empty() {
                                store.replace_import_edges(fid, &edges)?;
                                targets = store.import_target_paths(fid)?.into_iter().collect();
                            }
                        }
                    }
                }
            }
            targets
        }
        None => HashSet::new(),
    };

    // V3.G2 — call-site arg count (scorer only) + optional receiver type hint.
    let call_args = lang_info
        .and_then(|li| crate::intel::arity::call_arg_count_at(li.id, &read.bytes, line, col));
    let receiver_type = {
        let def_line = hit_occurrence
            .as_ref()
            .and_then(|h| h.local_def_ordinal)
            .and_then(|ord| {
                salt.and_then(|s| {
                    store
                        .occurrence_by_ordinal(&read.blob_hash, s, ord)
                        .ok()
                        .flatten()
                        .map(|d| d.line)
                })
            });
        // For method receivers the locals binding is on the receiver
        // token, not the method name — receiver_type_hint still helps when
        // the clicked token's own local def has a type annotation (e.g.
        // clicking a bare call's callee rarely). Best-effort.
        lang_info.and_then(|li| {
            crate::intel::arity::receiver_type_hint(li.id, &read.bytes, line, col, def_line)
        })
    };

    // (5) same repo — V3.G2 import-filtered cross-file arm.
    // Build name matches, tag by reach, rank, then arity-demote.
    let name_matches: Vec<(String, crate::extract::Symbol)> = repo_symbols
        .into_iter()
        .filter(|(_, s)| s.name == ident)
        .collect();
    let cand_paths: Vec<String> = name_matches.iter().map(|(p, _)| p.clone()).collect();
    let (tagged, _fuzzy) =
        crate::intel::cross_file::filter_and_tag(path, &cand_paths, &import_targets);

    let mut same_repo: Vec<Candidate> = Vec::with_capacity(tagged.len());
    for (idx, reach, class, precision) in tagged {
        let (p, s) = &name_matches[idx];
        let mut class = class;
        let mut precision = precision;
        // Arity demotion: never drop — only push class down to candidate.
        if let Some(argc) = call_args {
            let (sig_min, sig_max) = s
                .signature
                .as_deref()
                .map(crate::intel::arity::param_range_from_signature)
                .unwrap_or((None, None));
            let pmin = s.param_min.or(sig_min);
            let pmax = s.param_max.or(sig_max);
            if crate::intel::arity::arity_rejects(argc, pmin, pmax) && class == CLASS_LIKELY {
                class = CLASS_CANDIDATE;
                precision = PRECISION_TAGS_APPROX;
            }
        }
        let _ = reach; // ranking re-derives reach below for the full Candidate
        same_repo.push(candidate_with_class(
            repo.name.clone(),
            p.clone(),
            s.line_start,
            Some(s.kind.clone()),
            s.container.clone(),
            s.signature.clone(),
            s.doc.clone(),
            precision,
            class,
        ));
    }
    same_repo.sort_by(|a, b| {
        let a_reach = crate::intel::cross_file::classify_reach(path, &a.path, &import_targets);
        let b_reach = crate::intel::cross_file::classify_reach(path, &b.path, &import_targets);
        let a_boost = receiver_type
            .as_ref()
            .map(|t| a.container.as_deref() == Some(t.as_str()))
            .unwrap_or(false);
        let b_boost = receiver_type
            .as_ref()
            .map(|t| b.container.as_deref() == Some(t.as_str()))
            .unwrap_or(false);
        let a_same_c = a.container == current_container;
        let b_same_c = b.container == current_container;
        b_boost
            .cmp(&a_boost)
            .then_with(|| (a_reach as u8).cmp(&(b_reach as u8)))
            .then_with(|| b_same_c.cmp(&a_same_c))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    for c in same_repo {
        push_candidate(&mut candidates, &mut seen, c);
    }

    // (6) other repos, in configured order (always candidate — no cross-repo
    // import graph in v3.0).
    for other in repos {
        if other.name == repo.name {
            continue;
        }
        let Some(&other_id) = repo_ids.get(&other.name) else {
            continue;
        };
        let mut hits: Vec<Candidate> = store
            .symbols_for_repo(other_id)?
            .into_iter()
            .filter(|(_, s)| s.name == ident)
            .map(|(p, s)| {
                candidate(
                    other.name.clone(),
                    p,
                    s.line_start,
                    Some(s.kind),
                    s.container,
                    s.signature,
                    s.doc,
                    PRECISION_TAGS_APPROX,
                )
            })
            .collect();
        hits.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.line.cmp(&b.line)));
        for c in hits {
            push_candidate(&mut candidates, &mut seen, c);
        }
    }

    // V3.1-H2 — ensure EVERY tier carries signature/doc/container when a
    // symbols row exists at (path, name, line). Some early tiers enrich only
    // from the CURRENT blob; cross-file scip/locals misses still need a
    // symbols-table fill-in. Purely additive; never overwrites a present field.
    enrich_candidates_from_symbols(store, &ident, &mut candidates)?;

    candidates.truncate(MAX_CANDIDATES);
    let total = candidates.len();

    Ok(ResolveOut {
        schema: RESOLVE_SCHEMA,
        ident,
        position: Position { line, col },
        role,
        candidates,
        total,
        note: RESOLVE_NOTE,
    })
}

/// Fill missing `kind`/`container`/`signature`/`doc` on candidates from the
/// symbols table when a `(name, line_start)` row exists on that path.
/// Doc is trimmed to the first 40 lines (stored docs are usually already
/// whitespace-collapsed; this still bounds multi-line values).
fn enrich_candidates_from_symbols(
    store: &Store,
    ident: &str,
    candidates: &mut [Candidate],
) -> Result<(), ApiError> {
    let mut by_path: HashMap<String, Vec<crate::extract::Symbol>> = HashMap::new();
    for c in candidates.iter_mut() {
        // Always bound doc length even when already present.
        if c.doc.is_some() {
            c.doc = trim_doc_40(c.doc.take());
        }
        if c.kind.is_some() && c.container.is_some() && c.signature.is_some() && c.doc.is_some() {
            continue;
        }
        if !by_path.contains_key(&c.path) {
            let hits = store.symbols_at_path_named(&c.path, ident)?;
            by_path.insert(c.path.clone(), hits);
        }
        let hits = by_path.get(&c.path).cloned().unwrap_or_default();
        if let Some(s) = hits
            .iter()
            .find(|s| s.line_start == c.line || (s.line_start <= c.line && c.line <= s.line_end))
        {
            if c.kind.is_none() {
                c.kind = Some(s.kind.clone());
            }
            if c.container.is_none() {
                c.container = s.container.clone();
            }
            if c.signature.is_none() {
                c.signature = s.signature.clone();
            }
            if c.doc.is_none() {
                c.doc = trim_doc_40(s.doc.clone());
            }
        }
    }
    Ok(())
}

/// First 40 lines of a doc comment, trimmed. Stored docs are often a single
/// collapsed line; multi-line values (if any) are bounded here.
fn trim_doc_40(doc: Option<String>) -> Option<String> {
    let d = doc?;
    let joined: String = d.lines().take(40).collect::<Vec<_>>().join("\n");
    let t = joined.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Build a [`Candidate`] with `class` derived from `precision`.
#[allow(clippy::too_many_arguments)]
fn candidate(
    repo: String,
    path: String,
    line: u32,
    kind: Option<String>,
    container: Option<String>,
    signature: Option<String>,
    doc: Option<String>,
    precision: &'static str,
) -> Candidate {
    candidate_with_class(
        repo,
        path,
        line,
        kind,
        container,
        signature,
        doc,
        precision,
        class_for_precision(precision),
    )
}

/// Like [`candidate`] but with an explicit trust class (import-filter /
/// arity demotion may disagree with the precision→class default map).
#[allow(clippy::too_many_arguments)]
fn candidate_with_class(
    repo: String,
    path: String,
    line: u32,
    kind: Option<String>,
    container: Option<String>,
    signature: Option<String>,
    doc: Option<String>,
    precision: &'static str,
    class: &'static str,
) -> Candidate {
    Candidate {
        repo,
        path,
        line,
        kind,
        container,
        signature,
        doc,
        precision,
        class,
    }
}

/// Push `c` onto `candidates` unless its `(repo, path, line)` was already
/// emitted by a higher-priority tier — see the module doc's closing
/// paragraph on why the SAME location never appears twice.
fn push_candidate(
    candidates: &mut Vec<Candidate>,
    seen: &mut HashSet<(String, String, u32)>,
    c: Candidate,
) {
    if seen.insert((c.repo.clone(), c.path.clone(), c.line)) {
        candidates.push(c);
    }
}

/// The `[A-Za-z0-9_]` run touching byte offset `col` in `line` — the
/// no-occurrences-rows fallback (see the module doc). `col` may point
/// EITHER at the first byte of the word (the common case) OR one past its
/// last byte (a cursor resting right after a token still resolves that
/// token) — anything else (whitespace, punctuation, out of either word)
/// returns `None`.
pub(crate) fn word_at(line: &[u8], col: usize) -> Option<String> {
    fn is_word(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }
    let anchor = if col < line.len() && is_word(line[col]) {
        col
    } else if col > 0 && is_word(line[col - 1]) {
        col - 1
    } else {
        return None;
    };
    let mut start = anchor;
    let mut end = anchor + 1;
    while start > 0 && is_word(line[start - 1]) {
        start -= 1;
    }
    while end < line.len() && is_word(line[end]) {
        end += 1;
    }
    std::str::from_utf8(&line[start..end])
        .ok()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(root: &std::path::Path, path: &str, content: &str) {
        let abs = root.join(path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, content).unwrap();
    }

    fn fixture_repo(store: &Store, name: &str) -> (tempfile::TempDir, RepoEntry, i64) {
        let root = tempfile::tempdir().unwrap();
        let entry = RepoEntry {
            name: name.to_string(),
            path: root.path().to_path_buf(),
        };
        let repo_id = store
            .upsert_repo(name, root.path().to_str().unwrap())
            .unwrap();
        (root, entry, repo_id)
    }

    // --- word_at --------------------------------------------------------

    #[test]
    fn word_at_finds_the_touching_identifier_from_either_edge() {
        let line = b"  let widget = 1;";
        // Anchored on the first char of "widget".
        assert_eq!(word_at(line, 6), Some("widget".to_string()));
        // Anchored mid-word.
        assert_eq!(word_at(line, 9), Some("widget".to_string()));
        // Cursor resting one PAST the word's last byte.
        assert_eq!(word_at(line, 12), Some("widget".to_string()));
    }

    #[test]
    fn word_at_returns_none_on_punctuation_or_whitespace() {
        // Extra spaces around the punctuation so NEITHER edge (col nor
        // col - 1) lands on a word char — `word_at` deliberately also
        // resolves a cursor resting one byte PAST a word (see its own doc),
        // so a tight `"a + b;"` fixture would accidentally hit that rule.
        let line = b"a  +  b  ;";
        assert_eq!(word_at(line, 3), None); // the '+', isolated by spaces
        assert_eq!(word_at(line, 9), None); // the ';', isolated by spaces
    }

    // --- resolve_position: file-local ranks first ------------------------

    #[test]
    fn file_local_def_occurrence_ranks_first_and_is_enriched_from_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let src = "fn widget() -> i32 {\n    1\n}\n\nfn main() {\n    widget();\n}\n";
        write_file(root.path(), "a.rs", src);
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.rs", &blob_hash, "rust", src.len() as u64)
            .unwrap();
        let symbols = crate::extract::extract_symbols("rust", src.as_bytes()).unwrap();
        store
            .replace_symbols(&blob_hash, crate::lang::RUST.symbol_salt, &symbols)
            .unwrap();
        let occs = crate::occurrences::extract_occurrences("rust", src.as_bytes()).unwrap();
        store
            .replace_occurrences(&blob_hash, crate::lang::RUST.symbol_salt, &occs)
            .unwrap();

        // Click on the CALL SITE "widget();" — line 6, col 4 (0-based, on
        // the 'w').
        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.rs",
            6,
            4,
            None,
        )
        .unwrap();

        assert_eq!(out.schema, "resolve/1");
        assert_eq!(out.ident, "widget");
        assert_eq!(out.role.as_deref(), Some("ref"));
        assert!(!out.candidates.is_empty(), "got: {out:#?}");
        // V3.G1: the scope-proven locals binding ranks first (exact class);
        // same location as the old file-local def, enriched from symbols.
        let first = &out.candidates[0];
        assert_eq!(first.precision, "locals");
        assert_eq!(first.class, "exact");
        assert_eq!(first.line, 1);
        assert_eq!(first.kind.as_deref(), Some("fn"));
        assert!(out.note.contains("exact") || out.note.contains("candidate"));
    }

    // --- resolve_position: fallback path (no occurrences rows) ----------

    #[test]
    fn fallback_word_scan_when_the_blob_has_no_occurrences_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        // A Python file: `occurrences.rs` doesn't cover Python this phase,
        // so this blob will never have occurrences rows — the fallback
        // path must still resolve the word under the cursor.
        let src = "def widget():\n    return 1\n";
        write_file(root.path(), "a.py", src);
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.py", &blob_hash, "python", src.len() as u64)
            .unwrap();
        let symbols = crate::extract::extract_symbols("python", src.as_bytes()).unwrap();
        store
            .replace_symbols(&blob_hash, crate::lang::PYTHON.symbol_salt, &symbols)
            .unwrap();

        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.py",
            1,
            4,
            None,
        )
        .unwrap();
        assert_eq!(out.ident, "widget");
        assert_eq!(out.role, None, "fallback path never classifies a role");
        // Still finds the same-repo symbol match. V3.G2 may label a
        // same-file hit `import-filtered` (same-file/same-dir reach) rather
        // than plain `tags-approx` — either is a valid tags-tier hit.
        assert!(
            out.candidates.iter().any(|c| {
                c.line == 1 && (c.precision == "tags-approx" || c.precision == "import-filtered")
            }),
            "got: {:#?}",
            out.candidates
        );
    }

    // --- resolve_position: other-repo candidates -------------------------

    #[test]
    fn other_repo_matches_are_labeled_tags_approx_with_their_own_repo_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root_a, repo_a, repo_a_id) = fixture_repo(&store, "a");
        let (root_b, repo_b, repo_b_id) = fixture_repo(&store, "b");

        let src_a = "fn shared() -> i32 {\n    1\n}\n";
        write_file(root_a.path(), "a.rs", src_a);
        let hash_a = crate::ingest::git_blob_hash(src_a.as_bytes());
        store
            .upsert_file(repo_a_id, "a.rs", &hash_a, "rust", src_a.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &hash_a,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", src_a.as_bytes()).unwrap(),
            )
            .unwrap();

        let src_b = "fn shared() -> i32 {\n    2\n}\n";
        write_file(root_b.path(), "b.rs", src_b);
        let hash_b = crate::ingest::git_blob_hash(src_b.as_bytes());
        store
            .upsert_file(repo_b_id, "b.rs", &hash_b, "rust", src_b.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &hash_b,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", src_b.as_bytes()).unwrap(),
            )
            .unwrap();

        let repos = vec![repo_a.clone(), repo_b.clone()];
        let repo_ids = HashMap::from([
            (repo_a.name.clone(), repo_a_id),
            (repo_b.name.clone(), repo_b_id),
        ]);
        let out = resolve_position(
            &store, &repos, &repo_ids, &repo_a, repo_a_id, "a.rs", 1, 3, None,
        )
        .unwrap();
        assert!(out
            .candidates
            .iter()
            .any(|c| c.repo == "b" && c.precision == "tags-approx"));
    }

    // --- resolve_position: import-heuristic (B4) --------------------------

    #[test]
    fn import_heuristic_ranks_above_same_repo_tags_for_the_same_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let lib_src = "use b::widget;\n\nfn main() {\n    widget();\n}\n";
        write_file(root.path(), "src/lib.rs", lib_src);
        let lib_hash = crate::ingest::git_blob_hash(lib_src.as_bytes());
        store
            .upsert_file(
                repo_id,
                "src/lib.rs",
                &lib_hash,
                "rust",
                lib_src.len() as u64,
            )
            .unwrap();
        store
            .replace_symbols(
                &lib_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", lib_src.as_bytes()).unwrap(),
            )
            .unwrap();
        store
            .replace_occurrences(
                &lib_hash,
                crate::lang::RUST.symbol_salt,
                &crate::occurrences::extract_occurrences("rust", lib_src.as_bytes()).unwrap(),
            )
            .unwrap();

        let b_src = "pub fn widget() -> i32 {\n    42\n}\n";
        write_file(root.path(), "src/b.rs", b_src);
        let b_hash = crate::ingest::git_blob_hash(b_src.as_bytes());
        store
            .upsert_file(repo_id, "src/b.rs", &b_hash, "rust", b_src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &b_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", b_src.as_bytes()).unwrap(),
            )
            .unwrap();

        // A DECOY same-named symbol in a DIFFERENT directory (not same-dir,
        // not import-reachable) — proves the import-heuristic tier ranks
        // above the fleet-wide tags-approx tier. V3.G2's same-dir filter
        // would promote a `src/` sibling to import-filtered/likely, so the
        // decoy lives under `other/` to stay a pure global candidate.
        let c_src = "pub fn widget() -> i32 {\n    999\n}\n";
        write_file(root.path(), "other/c.rs", c_src);
        let c_hash = crate::ingest::git_blob_hash(c_src.as_bytes());
        store
            .upsert_file(repo_id, "other/c.rs", &c_hash, "rust", c_src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &c_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", c_src.as_bytes()).unwrap(),
            )
            .unwrap();

        // Click on the call site `widget();` — line 4, col 4.
        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "src/lib.rs",
            4,
            4,
            None,
        )
        .unwrap();

        assert_eq!(out.ident, "widget");
        assert!(!out.candidates.is_empty(), "got: {out:#?}");
        let first = &out.candidates[0];
        assert_eq!(first.precision, "import-heuristic");
        assert_eq!(first.path, "src/b.rs");
        assert_eq!(first.line, 1);
        assert_eq!(first.kind.as_deref(), Some("fn"));

        let decoy_index = out
            .candidates
            .iter()
            .position(|c| c.path == "other/c.rs")
            .expect("decoy should still appear, at the tags-approx tier");
        assert_eq!(out.candidates[decoy_index].precision, "tags-approx");
        assert_eq!(out.candidates[decoy_index].class, "candidate");
        assert!(decoy_index > 0, "import-heuristic hit must rank first");
        assert!(out.note.contains("import"));
    }

    #[test]
    fn import_heuristic_alias_click_resolves_to_the_original_name() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let lib_src = "use b::Original as Renamed;\n\nfn main() {}\n";
        write_file(root.path(), "src/lib.rs", lib_src);
        let lib_hash = crate::ingest::git_blob_hash(lib_src.as_bytes());
        store
            .upsert_file(
                repo_id,
                "src/lib.rs",
                &lib_hash,
                "rust",
                lib_src.len() as u64,
            )
            .unwrap();
        store
            .replace_symbols(
                &lib_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", lib_src.as_bytes()).unwrap(),
            )
            .unwrap();
        store
            .replace_occurrences(
                &lib_hash,
                crate::lang::RUST.symbol_salt,
                &crate::occurrences::extract_occurrences("rust", lib_src.as_bytes()).unwrap(),
            )
            .unwrap();

        let b_src = "pub struct Original;\n";
        write_file(root.path(), "src/b.rs", b_src);
        let b_hash = crate::ingest::git_blob_hash(b_src.as_bytes());
        store
            .upsert_file(repo_id, "src/b.rs", &b_hash, "rust", b_src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &b_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", b_src.as_bytes()).unwrap(),
            )
            .unwrap();

        // Click on the ALIAS itself in the `use` line: "use b::Original as
        // Renamed;" — `Renamed` starts at col 19.
        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "src/lib.rs",
            1,
            19,
            None,
        )
        .unwrap();
        assert_eq!(out.ident, "Renamed");
        let hit = out
            .candidates
            .iter()
            .find(|c| c.precision == "import-heuristic")
            .expect("alias should resolve via the import-heuristic tier");
        assert_eq!(hit.path, "src/b.rs");
        assert_eq!(hit.line, 1);
        assert_eq!(hit.kind.as_deref(), Some("struct"));
    }

    #[test]
    fn import_heuristic_unresolvable_module_falls_through_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        // `serde` is an external crate — nothing under this repo's `src/`
        // resolves it. Clicking the imported name must fall through
        // cleanly (zero candidates, no error), not panic or 500.
        let lib_src = "use serde::Deserialize;\n\nfn main() {}\n";
        write_file(root.path(), "src/lib.rs", lib_src);
        let lib_hash = crate::ingest::git_blob_hash(lib_src.as_bytes());
        store
            .upsert_file(
                repo_id,
                "src/lib.rs",
                &lib_hash,
                "rust",
                lib_src.len() as u64,
            )
            .unwrap();
        store
            .replace_symbols(
                &lib_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", lib_src.as_bytes()).unwrap(),
            )
            .unwrap();
        store
            .replace_occurrences(
                &lib_hash,
                crate::lang::RUST.symbol_salt,
                &crate::occurrences::extract_occurrences("rust", lib_src.as_bytes()).unwrap(),
            )
            .unwrap();

        // Click on `Deserialize`: "use serde::Deserialize;" — starts at
        // col 11.
        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "src/lib.rs",
            1,
            11,
            None,
        )
        .unwrap();
        assert_eq!(out.ident, "Deserialize");
        assert!(
            out.candidates.is_empty(),
            "an unresolvable import must fall through with zero candidates, not error: {out:#?}"
        );
    }

    #[test]
    fn import_heuristic_costs_one_extra_parse_measured() {
        // Cost note (module doc): tier (a.5) pays one extra tree-sitter
        // parse of the CURRENT file only when tier (a) file-local found
        // nothing. Not a hard perf assertion (CI machines vary widely) —
        // prints the measured wall time for `cargo test -p kb-code-server
        // import_heuristic_costs_one_extra_parse_measured -- --nocapture`
        // to carry a real number into the phase report.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let lib_src = "use b::widget;\n\nfn main() {\n    widget();\n}\n";
        write_file(root.path(), "src/lib.rs", lib_src);
        let lib_hash = crate::ingest::git_blob_hash(lib_src.as_bytes());
        store
            .upsert_file(
                repo_id,
                "src/lib.rs",
                &lib_hash,
                "rust",
                lib_src.len() as u64,
            )
            .unwrap();
        store
            .replace_symbols(
                &lib_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", lib_src.as_bytes()).unwrap(),
            )
            .unwrap();
        store
            .replace_occurrences(
                &lib_hash,
                crate::lang::RUST.symbol_salt,
                &crate::occurrences::extract_occurrences("rust", lib_src.as_bytes()).unwrap(),
            )
            .unwrap();

        let b_src = "pub fn widget() -> i32 {\n    42\n}\n";
        write_file(root.path(), "src/b.rs", b_src);
        let b_hash = crate::ingest::git_blob_hash(b_src.as_bytes());
        store
            .upsert_file(repo_id, "src/b.rs", &b_hash, "rust", b_src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &b_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", b_src.as_bytes()).unwrap(),
            )
            .unwrap();

        let repos = std::slice::from_ref(&repo);
        let repo_ids = HashMap::from([(repo.name.clone(), repo_id)]);
        let iterations = 200u32;
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            resolve_position(
                &store,
                repos,
                &repo_ids,
                &repo,
                repo_id,
                "src/lib.rs",
                4,
                4,
                None,
            )
            .unwrap();
        }
        let elapsed = start.elapsed();
        eprintln!(
            "import-heuristic: {iterations} resolve_position calls (each exercising the extra \
             tree-sitter parse) in {elapsed:?} — {:?}/call average",
            elapsed / iterations
        );
    }

    // --- resolve_position: scip-exact (S1) --------------------------------

    #[test]
    fn scip_exact_ranks_first_above_file_local_and_a_tags_approx_decoy() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let src = "fn widget() -> i32 {\n    1\n}\n\nfn main() {\n    widget();\n}\n";
        write_file(root.path(), "a.rs", src);
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.rs", &blob_hash, "rust", src.len() as u64)
            .unwrap();
        let symbols = crate::extract::extract_symbols("rust", src.as_bytes()).unwrap();
        store
            .replace_symbols(&blob_hash, crate::lang::RUST.symbol_salt, &symbols)
            .unwrap();
        let occs = crate::occurrences::extract_occurrences("rust", src.as_bytes()).unwrap();
        store
            .replace_occurrences(&blob_hash, crate::lang::RUST.symbol_salt, &occs)
            .unwrap();

        // A DECOY same-named symbol in a different directory — pure global
        // tags-approx (same-dir would promote it under V3.G2).
        let decoy_src = "pub fn widget() -> i32 {\n    999\n}\n";
        write_file(root.path(), "other/decoy.rs", decoy_src);
        let decoy_hash = crate::ingest::git_blob_hash(decoy_src.as_bytes());
        store
            .upsert_file(
                repo_id,
                "other/decoy.rs",
                &decoy_hash,
                "rust",
                decoy_src.len() as u64,
            )
            .unwrap();
        store
            .replace_symbols(
                &decoy_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", decoy_src.as_bytes()).unwrap(),
            )
            .unwrap();

        // A SCIP def occurrence for "widget" at line 1 — the SAME line the
        // `ts` pass already found (proves the scip row both ranks first
        // AND suppresses the file-local duplicate at the same location).
        store
            .replace_scip_occurrences(
                &blob_hash,
                crate::lang::RUST.symbol_salt,
                &[crate::store::ScipOccurrenceIn {
                    name: "widget".to_string(),
                    role: "def".to_string(),
                    line: 1,
                    col_start: 3,
                    col_end: 9,
                }],
            )
            .unwrap();

        // Click on the call site "widget();" — line 6, col 4.
        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.rs",
            6,
            4,
            None,
        )
        .unwrap();

        assert_eq!(out.ident, "widget");
        assert!(!out.candidates.is_empty(), "got: {out:#?}");
        let first = &out.candidates[0];
        assert_eq!(first.precision, "scip-exact");
        assert_eq!(first.line, 1);
        // Only ONE candidate at (a.rs, line 1) — the ts file-local
        // duplicate at the identical location was suppressed by `seen`.
        assert_eq!(
            out.candidates
                .iter()
                .filter(|c| c.path == "a.rs" && c.line == 1)
                .count(),
            1,
            "got: {:#?}",
            out.candidates
        );

        let decoy_index = out
            .candidates
            .iter()
            .position(|c| c.path == "other/decoy.rs")
            .expect("decoy should still appear, at the tags-approx tier");
        assert_eq!(out.candidates[decoy_index].precision, "tags-approx");
        assert_eq!(out.candidates[decoy_index].class, "candidate");
        assert!(decoy_index > 0, "scip-exact hit must rank first");
        assert!(out.note.contains("scip"));
    }

    #[test]
    fn scip_rows_make_the_position_lookup_itself_exact() {
        // A blob whose ONLY occurrence rows are `scip` (as if a language
        // `occurrences.rs` doesn't cover was still `scip ingest`-ed) — the
        // fallback-vs-lookup branch must still take the occurrence-lookup
        // path (`has_occurrences` is source-agnostic), not the plain
        // word-scan.
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let src = "fn widget() -> i32 {\n    1\n}\n";
        write_file(root.path(), "a.rs", src);
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.rs", &blob_hash, "rust", src.len() as u64)
            .unwrap();
        store
            .replace_scip_occurrences(
                &blob_hash,
                crate::lang::RUST.symbol_salt,
                &[crate::store::ScipOccurrenceIn {
                    name: "widget".to_string(),
                    role: "def".to_string(),
                    line: 1,
                    col_start: 3,
                    col_end: 9,
                }],
            )
            .unwrap();

        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.rs",
            1,
            4,
            None,
        )
        .unwrap();
        assert_eq!(out.ident, "widget");
        assert_eq!(
            out.role.as_deref(),
            Some("def"),
            "the scip occurrence's own role must be used, not a word-scan None"
        );
    }

    // --- resolve_position: framework-convention (PRR-N5) ------------------

    #[test]
    fn framework_convention_ranks_above_tags_approx_for_a_recognized_construct() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        // The clicked ERB view — line 1 is a recognized `render` construct.
        // The framework tier is position-only: these bytes are never
        // parsed by this test, only read to compute a blob hash + let
        // `word_at`'s fallback find SOME identifier at the click position.
        let erb_path = "app/views/x/index.html.erb";
        write_file(root.path(), erb_path, "<%= render \"row\" %>\n");

        // A DECOY same-named symbol elsewhere in the repo — proves the
        // framework-convention tier ranks ABOVE the fleet-wide tags-approx
        // tier for the identical name (same style as the scip-exact /
        // import-heuristic decoy tests above).
        let decoy_src = "fn row() -> i32 {\n    1\n}\n";
        write_file(root.path(), "other/decoy.rs", decoy_src);
        let decoy_hash = crate::ingest::git_blob_hash(decoy_src.as_bytes());
        store
            .upsert_file(
                repo_id,
                "other/decoy.rs",
                &decoy_hash,
                "rust",
                decoy_src.len() as u64,
            )
            .unwrap();
        store
            .replace_symbols(
                &decoy_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", decoy_src.as_bytes()).unwrap(),
            )
            .unwrap();

        // The rails_edges row itself, at the SAME position (line 1) the
        // click below lands on.
        let erb_bytes = std::fs::read(root.path().join(erb_path)).unwrap();
        let erb_hash = crate::ingest::git_blob_hash(&erb_bytes);
        store
            .replace_rails_edges(
                repo_id,
                erb_path,
                &erb_hash,
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &[crate::frameworks::FrameworkEdge {
                    kind: crate::frameworks::EdgeKind::RenderPartial,
                    src_path: erb_path.to_string(),
                    src_line: Some(1),
                    src_symbol: None,
                    dst_kind: Some("partial".to_string()),
                    dst_path: Some("app/views/x/_row.html.erb".to_string()),
                    dst_symbol: None,
                    trust: crate::frameworks::Trust::Likely,
                    extra_json: None,
                }],
            )
            .unwrap();

        // Click on `"row"` inside the string literal — col 12 lands on the
        // 'r' (`<%= render "row" %>`: 'r' of "row" is byte offset 12).
        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            erb_path,
            1,
            12,
            None,
        )
        .unwrap();

        assert_eq!(out.ident, "row");
        assert!(!out.candidates.is_empty(), "got: {out:#?}");
        let first = &out.candidates[0];
        assert_eq!(first.precision, PRECISION_FRAMEWORK);
        assert_eq!(first.class, "likely");
        assert_eq!(first.path, "app/views/x/_row.html.erb");
        assert_eq!(first.line, 1);

        let decoy_index = out
            .candidates
            .iter()
            .position(|c| c.path == "other/decoy.rs")
            .expect("decoy should still appear, at the tags-approx tier");
        assert_eq!(out.candidates[decoy_index].precision, "tags-approx");
        assert!(
            decoy_index > 0,
            "framework-convention hit must rank first: {out:#?}"
        );
    }

    #[test]
    fn framework_convention_is_absent_for_a_non_rails_relevant_path() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");

        let src = "fn widget() -> i32 {\n    1\n}\n";
        write_file(root.path(), "a.rs", src);
        let blob_hash = crate::ingest::git_blob_hash(src.as_bytes());
        store
            .upsert_file(repo_id, "a.rs", &blob_hash, "rust", src.len() as u64)
            .unwrap();
        store
            .replace_symbols(
                &blob_hash,
                crate::lang::RUST.symbol_salt,
                &crate::extract::extract_symbols("rust", src.as_bytes()).unwrap(),
            )
            .unwrap();
        // A rails_edges row keyed on "a.rs" anyway — proves the tier's own
        // `rails_lens_relevant_path` gate, not just an absence of data.
        store
            .replace_rails_edges(
                repo_id,
                "a.rs",
                &blob_hash,
                crate::frameworks::RAILS_LENS_GRAMMAR_VERSION,
                &[crate::frameworks::FrameworkEdge {
                    kind: crate::frameworks::EdgeKind::RenderPartial,
                    src_path: "a.rs".to_string(),
                    src_line: Some(1),
                    src_symbol: None,
                    dst_kind: Some("partial".to_string()),
                    dst_path: Some("somewhere/else.erb".to_string()),
                    dst_symbol: None,
                    trust: crate::frameworks::Trust::Likely,
                    extra_json: None,
                }],
            )
            .unwrap();

        let out = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.rs",
            1,
            3,
            None,
        )
        .unwrap();
        assert!(
            !out.candidates
                .iter()
                .any(|c| c.precision == PRECISION_FRAMEWORK),
            "got: {out:#?}"
        );
    }

    // --- 400s -------------------------------------------------------------

    #[test]
    fn out_of_range_line_400s() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");
        write_file(root.path(), "a.rs", "fn a() {}\n");
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();

        let err = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.rs",
            999,
            0,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("out of range"));
    }

    #[test]
    fn out_of_range_col_400s() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");
        write_file(root.path(), "a.rs", "fn a() {}\n");
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();

        let err = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.rs",
            1,
            999,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("out of range"));
    }

    #[test]
    fn no_identifier_at_position_400s() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (root, repo, repo_id) = fixture_repo(&store, "fixture");
        let src = "fn a() {}\n";
        write_file(root.path(), "a.rs", src);
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", src.len() as u64)
            .unwrap();

        // col 6 is the space between "a()" and "{}" — no identifier there
        // (nor does the byte just before it, ')', count as one either).
        let err = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "a.rs",
            1,
            6,
            None,
        )
        .unwrap_err();
        assert!(format!("{err:?}").contains("no identifier"));
    }

    #[test]
    fn missing_file_404s() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        let (_root, repo, repo_id) = fixture_repo(&store, "fixture");
        let err = resolve_position(
            &store,
            std::slice::from_ref(&repo),
            &HashMap::from([(repo.name.clone(), repo_id)]),
            &repo,
            repo_id,
            "does-not-exist.rs",
            1,
            0,
            None,
        )
        .unwrap_err();
        // `read_repo_file` maps a missing working-tree file to 404 — see
        // `routes.rs`.
        assert!(format!("{err:?}").contains("not found"));
    }
}
