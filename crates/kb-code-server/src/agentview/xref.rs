//! `GET /api/defs?repo=&symbol=` + `GET /api/xrefs?repo=&symbol=` (W5.2) —
//! a symbols-table lookup and a plain text-grep, respectively (`xrefs`, not
//! `refs` — `/api/refs` was independently claimed by W4.1's git ref-picker,
//! `routes::refs`; see `router.rs`'s module doc for how this cherry-pick
//! resolved the collision). Both are
//! deliberately TAGS-TIER (`extract.rs`'s tree-sitter `tags.scm` symbol
//! table for `defs`, `grep-searcher` text matching for `refs`), never a
//! real reference-graph / scope resolution — see each fn's own doc for the
//! exact honesty labeling this module carries in its JSON.
//!
//! # defs
//!
//! An EXACT (case-sensitive) name match against every symbol
//! `store::Store::symbols_for_repo` currently indexes, across every
//! configured repo unless `?repo=` narrows it (mirrors `GET /api/search/
//! {files,symbols}`'s own "all repos by default" convention — a defs
//! lookup is a cheap in-memory scan, same cost profile as those lanes).
//! When the exact pass finds NOTHING, a fuzzy fallback runs instead
//! (`state.symbol_index`, the SAME nucleo lane `GET /api/search/symbols`
//! uses) — every fuzzy hit is labeled `"approximate": true`; every exact
//! hit is `"approximate": false`. `exact` on the response says which pass
//! actually produced `results`.
//!
//! # refs
//!
//! Word-boundary text search for `symbol` across ONE repo's working tree
//! (`?repo=` is REQUIRED here — mirrors `search::text`'s own "no
//! multi-repo fan-out" scope limit), reusing `search::text::search_text`
//! verbatim with a `\b<symbol>\b` regex pattern
//! ([`super::word_boundary_pattern`]). EVERY result is `"approximate":
//! true` — this is plain text matching, not symbol resolution: a hit may
//! land inside a comment, a string literal, or an unrelated identically-
//! named symbol in a completely different scope. [`REFS_NOTE`] carries
//! that same honesty note in the wire response itself, not just this doc
//! comment.

use crate::extract::Symbol;
use crate::routes::{clamp_limit, find_repo, resolve_search_repos, ApiError};
use crate::search::{self, SymbolIndex};
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const DEFS_SCHEMA: &str = "defs/1";
pub const REFS_SCHEMA: &str = "refs/1";

/// Carried verbatim in every `refs/1` response — see the module doc.
pub const REFS_NOTE: &str = "text-grep, tags-tier match — NOT semantic/scope resolution; may \
     include comments, string literals, or an unrelated identically-named symbol";

// --- defs -------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DefsParams {
    pub repo: Option<String>,
    pub symbol: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DefHit {
    pub repo: String,
    pub path: String,
    #[serde(flatten)]
    pub symbol: Symbol,
    pub approximate: bool,
    /// Trust class (V3.G1) — defs is name-based, so results are
    /// `"candidate"` (never scope-proven exact). Additive wire field.
    pub class: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DefsOut {
    pub schema: &'static str,
    pub symbol: String,
    /// `true` when `results` came from the exact-name pass; `false` means
    /// the exact pass found nothing and every result is a fuzzy fallback
    /// (all `approximate: true`).
    pub exact: bool,
    pub results: Vec<DefHit>,
}

/// `GET /api/defs?repo=&symbol=[&limit=]`.
pub async fn defs_route(
    State(state): State<SharedState>,
    Query(params): Query<DefsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let symbol = params.symbol.trim();
    if symbol.is_empty() {
        return Err(ApiError::bad_request("symbol must not be empty"));
    }
    let repos = resolve_search_repos(&state, params.repo.as_deref())?;
    let limit = clamp_limit(params.limit);
    // `resolve_defs` keeps its own `&Store` signature (a sync helper,
    // directly unit-testable) — the wrap happens here, at the async
    // boundary (store.rs's 2026-08-31 incident note).
    let symbol_index = state.symbol_index.clone();
    let symbol_owned = symbol.to_string();
    let out = state
        .store
        .run_blocking(move |store| resolve_defs(store, &symbol_index, &repos, &symbol_owned, limit))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `pub(crate)` — narrow deps (`&Store`/`&SymbolIndex`, not `SharedState`)
/// so this is directly unit-testable against a fixture store with no
/// daemon boot.
pub(crate) fn resolve_defs(
    store: &Store,
    symbol_index: &SymbolIndex,
    repos: &[(String, i64)],
    symbol: &str,
    limit: usize,
) -> Result<DefsOut, ApiError> {
    let mut exact: Vec<DefHit> = Vec::new();
    for (repo_name, repo_id) in repos {
        for (path, sym) in store.symbols_for_repo(*repo_id)? {
            if sym.name == symbol {
                exact.push(DefHit {
                    repo: repo_name.clone(),
                    path,
                    symbol: sym,
                    approximate: false,
                    // Name-table match only — not scope-proven.
                    class: crate::resolve::CLASS_CANDIDATE,
                });
            }
        }
    }
    exact.sort_by(|a, b| a.repo.cmp(&b.repo).then_with(|| a.path.cmp(&b.path)));
    exact.truncate(limit);

    if !exact.is_empty() {
        return Ok(DefsOut {
            schema: DEFS_SCHEMA,
            symbol: symbol.to_string(),
            exact: true,
            results: exact,
        });
    }

    let fuzzy = symbol_index.search(store, repos, symbol, limit, None)?;
    let results: Vec<DefHit> = fuzzy
        .into_iter()
        .map(|h| DefHit {
            repo: h.repo,
            path: h.path,
            symbol: h.symbol,
            approximate: true,
            class: crate::resolve::CLASS_CANDIDATE,
        })
        .collect();
    Ok(DefsOut {
        schema: DEFS_SCHEMA,
        symbol: symbol.to_string(),
        exact: false,
        results,
    })
}

// --- refs ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RefsParams {
    pub repo: String,
    pub symbol: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RefHit {
    pub path: String,
    pub line: u64,
    pub text: String,
    pub approximate: bool,
    /// Trust class (V3.G1) — text-grep is `"candidate"` by default;
    /// upgraded to `"likely"` when the hit file imports `symbol` per the
    /// existing [`crate::imports::import_origin`] helper (no new import
    /// machinery — G2).
    pub class: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RefsOut {
    pub schema: &'static str,
    pub repo: String,
    pub symbol: String,
    pub results: Vec<RefHit>,
    pub truncated: bool,
    /// The search hit [`REFS_TIME_BUDGET`] before finishing — an HONEST
    /// "there may be more (or ANY) matches we didn't get to look for"
    /// signal, kept as its own field rather than folded into `truncated`
    /// (mirrors `routes::search_text_route`'s own `truncated`/
    /// `time_budget_exceeded` split — the two conditions have different
    /// causes and a caller may want to react to them differently).
    pub time_budget_exceeded: bool,
    pub note: &'static str,
}

/// `GET /api/xrefs?repo=&symbol=[&limit=]`.
pub async fn refs_route(
    State(state): State<SharedState>,
    Query(params): Query<RefsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let symbol = params.symbol.trim();
    if symbol.is_empty() {
        return Err(ApiError::bad_request("symbol must not be empty"));
    }
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let limit = clamp_limit(params.limit);
    // `resolve_refs` (text-grep + a per-file fs read for the import-class
    // upgrade) keeps its own `&Store` signature — wrap at the async
    // boundary, same as `defs_route` above.
    let repo_root = repo.path.clone();
    let repo_name = repo.name.clone();
    let symbol_owned = symbol.to_string();
    let out = state
        .store
        .run_blocking(move |store| {
            resolve_refs(store, &repo_root, &repo_name, repo_id, &symbol_owned, limit)
        })
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `pub(crate)` — narrow deps so this is directly unit-testable against a
/// fixture store + a real (tiny) working tree, no daemon boot, no git
/// repository required (`search::text::search_text` never shells out to
/// git — see that module's own doc).
pub(crate) fn resolve_refs(
    store: &Store,
    repo_root: &std::path::Path,
    repo_name: &str,
    repo_id: i64,
    symbol: &str,
    limit: usize,
) -> Result<RefsOut, ApiError> {
    let pattern = super::word_boundary_pattern(symbol);
    let resp = search::search_text(
        store,
        repo_root,
        repo_id,
        &pattern,
        true, // regex
        true, // case-sensitive — an identifier match, not prose
        super::TEXT_SEARCH_TIME_BUDGET,
        None,
    )?;

    // Per-file import signal (V3.G1): if the hit file imports `symbol`,
    // class upgrades candidate → likely. Uses the existing import-origin
    // helper only — no new cross-file machinery (lane G2).
    let mut import_class_cache: std::collections::HashMap<String, &'static str> =
        std::collections::HashMap::new();

    let mut results: Vec<RefHit> = Vec::new();
    'outer: for file in &resp.results {
        let class = *import_class_cache
            .entry(file.path.clone())
            .or_insert_with(|| xref_class_for_file(repo_root, &file.path, symbol));
        for m in &file.matches {
            results.push(RefHit {
                path: file.path.clone(),
                line: m.line_no,
                text: m.line.clone(),
                approximate: true,
                class,
            });
            if results.len() >= limit {
                break 'outer;
            }
        }
    }
    let truncated = resp.truncated || results.len() >= limit;

    Ok(RefsOut {
        schema: REFS_SCHEMA,
        repo: repo_name.to_string(),
        symbol: symbol.to_string(),
        results,
        truncated,
        time_budget_exceeded: resp.time_budget_exceeded,
        note: REFS_NOTE,
    })
}

/// `"likely"` when `path` imports `symbol` via the existing import ladder;
/// `"candidate"` otherwise (including unreadable/unsupported files).
fn xref_class_for_file(repo_root: &std::path::Path, path: &str, symbol: &str) -> &'static str {
    let abs = repo_root.join(path);
    let Ok(bytes) = std::fs::read(&abs) else {
        return crate::resolve::CLASS_CANDIDATE;
    };
    let Some(lang) = crate::lang::detect(path, Some(&bytes)) else {
        return crate::resolve::CLASS_CANDIDATE;
    };
    if !crate::imports::supports(lang.id) {
        return crate::resolve::CLASS_CANDIDATE;
    }
    if crate::imports::import_origin(lang.id, &bytes, symbol).is_some() {
        crate::resolve::CLASS_LIKELY
    } else {
        crate::resolve::CLASS_CANDIDATE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Symbol;

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    fn sym(name: &str) -> Symbol {
        Symbol {
            ordinal: 0,
            name: name.to_string(),
            kind: "fn".to_string(),
            line_start: 1,
            line_end: 2,
            col_start: 0,
            col_end: 1,
            container: None,
            signature: None,
            doc: None,
            param_min: None,
            param_max: None,
        }
    }

    #[test]
    fn defs_exact_match_is_not_approximate() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashA", "rust@1", &[sym("run")])
            .unwrap();
        let index = SymbolIndex::new();

        let out = resolve_defs(&store, &index, &[("r".into(), repo_id)], "run", 10).unwrap();
        assert!(out.exact);
        assert_eq!(out.results.len(), 1);
        assert!(!out.results[0].approximate);
        assert_eq!(out.results[0].symbol.name, "run");
    }

    #[test]
    fn defs_falls_back_to_fuzzy_when_no_exact_match() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashA", "rust@1", &[sym("runner")])
            .unwrap();
        let index = SymbolIndex::new();

        let out = resolve_defs(&store, &index, &[("r".into(), repo_id)], "run", 10).unwrap();
        assert!(!out.exact);
        assert!(!out.results.is_empty());
        assert!(out.results.iter().all(|h| h.approximate));
        assert!(out.results.iter().any(|h| h.symbol.name == "runner"));
    }

    #[test]
    fn defs_no_match_anywhere_is_an_empty_fuzzy_result() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashA", "rust@1", &[sym("totally_unrelated")])
            .unwrap();
        let index = SymbolIndex::new();

        let out = resolve_defs(&store, &index, &[("r".into(), repo_id)], "zzz_nope", 10).unwrap();
        assert!(!out.exact);
        assert!(out.results.is_empty());
    }

    fn write_file(root: &std::path::Path, path: &str, content: &str) {
        let abs = root.join(path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, content).unwrap();
    }

    #[test]
    fn refs_word_boundary_does_not_match_a_longer_identifier() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        write_file(
            root.path(),
            "a.rs",
            "fn foo() {}\nfn foobar() {}\nlet x = foo();\n",
        );
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();

        let out = resolve_refs(&store, root.path(), "r", repo_id, "foo", 10).unwrap();
        assert_eq!(out.results.len(), 2, "got: {:#?}", out.results);
        assert!(out.results.iter().all(|r| r.approximate));
        assert!(out.results.iter().all(|r| !r.text.contains("foobar")));
        assert!(out.results.iter().any(|r| r.text.contains("fn foo()")));
        assert!(out.results.iter().any(|r| r.text.contains("let x = foo()")));
    }

    #[test]
    fn refs_no_match_is_empty_not_an_error() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        write_file(root.path(), "a.rs", "fn other() {}\n");
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();

        let out = resolve_refs(&store, root.path(), "r", repo_id, "zzz_nope", 10).unwrap();
        assert!(out.results.is_empty());
        assert!(!out.truncated);
    }
}
