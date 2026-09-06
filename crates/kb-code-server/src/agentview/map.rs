//! `GET /api/map?repo=&path=&budget=` + `kb-code map` (W5.1) — a ranked,
//! token-budgeted outline of a directory/repo: aider-repomap-style, but
//! deliberately simplified — see "v1-rank" below.
//!
//! # Symbol collection
//!
//! Every CURRENT symbol reachable from `repo_id`'s `files` rows
//! (`store::Store::symbols_for_repo`), EXCLUDING key-path outline rows
//! (`extract::is_repo_map_symbol` — YAML/TOML/JSON's `"key"` kind: a
//! key-path dump isn't the "here's what this file DOES" signal a repo map
//! wants), narrowed to files under `?path=` (a directory PREFIX, compared
//! component-wise — `"src/foo"` matches `"src/foo/bar.rs"` but never
//! `"src/foobar.rs"`) when given; an empty `path` (the default) matches
//! every file.
//!
//! # v1-rank (graph-rank deferred)
//!
//! ```text
//! rank = frecency_boost(path)
//!      + SYMBOL_COUNT_WEIGHT * qualifying_symbol_count(path)
//!      - DEPTH_PENALTY_PER_SEGMENT * path_depth(path)
//! ```
//!
//! - `frecency_boost` — the SAME exponential-decay open-history curve
//!   `search::files::FileIndex`'s own frecency blend uses
//!   (`search::files::recency_boost`, reused verbatim rather than
//!   re-derived — one frecency curve for the whole daemon; `store::Store::
//!   last_opened_map` is the underlying signal).
//! - `qualifying_symbol_count` — how many indexed, non-`"key"` symbols a
//!   file has (more indexed surface ⇒ a more central file worth
//!   summarizing).
//! - `path_depth` — `/`-separated segment count before the filename (a
//!   mild bias toward files nearer the repo root).
//!
//! This is **v1-rank, not graph-rank**: a real symbol-reference-count
//! signal (which files' symbol NAMES are actually mentioned by other
//! files' code — the design brief's original ask) is deliberately
//! DEFERRED. It needs either the semantic chunker's parse output threaded
//! through a second cross-file pass, or a dedicated xref index, neither of
//! which exists at this Wave (`xref::refs` is a per-query grep, not a
//! precomputed graph). Recorded here so a future pass knows exactly what
//! "graph-rank" would replace, and why v1 stops short of it.
//!
//! # Budget fill
//!
//! Files are visited rank-descending; each file's WHOLE outline entry
//! (a header line plus one line per symbol) is appended to the response
//! while the running `chars/4` token estimate ([`super::approx_tokens`])
//! stays at or under `budget`. The first candidate whose own entry would
//! push the running total over `budget` stops the fill —
//! `truncated: true` — rather than emitting a partial per-file entry: a
//! whole file's outline is the smallest unit this pass ever cuts.

use super::{approx_tokens, path_depth};
use crate::extract::{is_repo_map_symbol, Symbol};
use crate::routes::{find_repo, safe_rel_path, ApiError};
use crate::search::files::recency_boost;
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCHEMA: &str = "map/1";
/// Default `?budget=` (approx tokens) when omitted — generous enough for a
/// medium-sized directory's outline in one call.
pub const DEFAULT_BUDGET: usize = 2_000;
/// Defensive ceiling on an operator-supplied `?budget=` — mirrors
/// `provenance::report::MAX_ALLOWED_MAX_COUNT`'s "a runaway request can't
/// make this call unbounded" precedent.
pub const MAX_BUDGET: usize = 200_000;

/// Additive rank contribution per qualifying symbol — see the module doc's
/// formula. Comparable in magnitude to a single fresh open's frecency
/// boost (`search::files::FRECENCY_BOOST_MAX` = 60): a file with ~30
/// symbols outranks a just-opened, symbol-sparse file, without a handful
/// of symbols alone ever swamping a strong frecency signal.
const SYMBOL_COUNT_WEIGHT: f64 = 2.0;
/// Rank penalty per `/`-separated path segment — see the module doc.
const DEPTH_PENALTY_PER_SEGMENT: f64 = 3.0;

#[derive(Debug, Deserialize)]
pub struct MapParams {
    pub repo: String,
    #[serde(default)]
    pub path: String,
    pub budget: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapSymbolOut {
    pub name: String,
    pub kind: String,
    pub line_start: u32,
    pub line_end: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

impl From<&Symbol> for MapSymbolOut {
    fn from(s: &Symbol) -> Self {
        Self {
            name: s.name.clone(),
            kind: s.kind.clone(),
            line_start: s.line_start,
            line_end: s.line_end,
            container: s.container.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapFileOut {
    pub path: String,
    pub rank: f64,
    pub symbols: Vec<MapSymbolOut>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MapOut {
    pub schema: &'static str,
    pub repo: String,
    pub path: String,
    pub budget: usize,
    pub used_tokens: usize,
    pub truncated: bool,
    pub files: Vec<MapFileOut>,
    /// The fully rendered outline text — `files` in JSON form, plus this
    /// pre-rendered string so a caller (agent or human) never has to
    /// re-derive the exact same layout `kb-code map`'s human-mode print
    /// already committed to.
    pub outline: String,
}

/// `GET /api/map?repo=&path=&budget=`.
pub async fn map_route(
    State(state): State<SharedState>,
    Query(params): Query<MapParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let scope = safe_rel_path(&params.path)?.to_string();
    let budget = params.budget.unwrap_or(DEFAULT_BUDGET).clamp(1, MAX_BUDGET);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let repo_name = repo.name.clone();

    // `build_map` is a store read plus pure CPU ranking/budget-fill work —
    // one blocking-pool round trip (store.rs's 2026-08-31 incident note).
    let out = state
        .store
        .run_blocking(move |store| build_map(store, repo_id, &repo_name, &scope, budget, now_ms))
        .await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `pub(crate)` — [`super::pack`] reuses this to render ONE file's own
/// outline entry (`scope` = that exact path, `budget = usize::MAX` so a
/// single-file entry is never itself truncated — pack's own budget gate is
/// about FILE CONTENT, never a file's outline; see that module's doc).
/// Takes a narrow `&Store` (not `SharedState`) so it's directly
/// unit-testable against a fixture store with no daemon boot.
pub(crate) fn build_map(
    store: &Store,
    repo_id: i64,
    repo_name: &str,
    scope: &str,
    budget: usize,
    now_ms: i64,
) -> Result<MapOut, ApiError> {
    let all = store.symbols_for_repo(repo_id)?;
    let recency = store.last_opened_map(repo_id)?;

    let mut by_path: BTreeMap<String, Vec<&Symbol>> = BTreeMap::new();
    for (path, symbol) in &all {
        if !is_repo_map_symbol(&symbol.kind) {
            continue;
        }
        if !path_in_scope(path, scope) {
            continue;
        }
        by_path.entry(path.clone()).or_default().push(symbol);
    }

    let mut ranked: Vec<(String, f64, Vec<&Symbol>)> = by_path
        .into_iter()
        .map(|(path, mut symbols)| {
            symbols.sort_by_key(|s| s.line_start);
            let boost = recency
                .get(&path)
                .map(|&opened_at| recency_boost(now_ms, opened_at))
                .unwrap_or(0.0);
            let rank = boost + SYMBOL_COUNT_WEIGHT * symbols.len() as f64
                - DEPTH_PENALTY_PER_SEGMENT * path_depth(&path) as f64;
            (path, rank, symbols)
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut files = Vec::new();
    let mut outline = String::new();
    let mut used_tokens = 0usize;
    let mut truncated = false;
    for (path, rank, symbols) in ranked {
        let entry_text = render_file_entry(&path, &symbols);
        let entry_tokens = approx_tokens(&entry_text);
        if used_tokens.saturating_add(entry_tokens) > budget {
            truncated = true;
            break;
        }
        outline.push_str(&entry_text);
        used_tokens += entry_tokens;
        files.push(MapFileOut {
            path,
            rank,
            symbols: symbols.iter().map(|s| MapSymbolOut::from(*s)).collect(),
        });
    }

    Ok(MapOut {
        schema: SCHEMA,
        repo: repo_name.to_string(),
        path: scope.to_string(),
        budget,
        used_tokens,
        truncated,
        files,
        outline,
    })
}

/// `true` when `path` is `scope` itself or lives under it, compared
/// component-wise — see the module doc. `scope == ""` (the repo root,
/// `safe_rel_path`'s own default) matches every path.
fn path_in_scope(path: &str, scope: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    path == scope || path.starts_with(&format!("{scope}/"))
}

/// One file's rendered outline entry: a bare path header line, then one
/// indented `"{kind} {name}{ (container)} L{start}-{end}"` line per
/// symbol, in line-ascending order.
fn render_file_entry(path: &str, symbols: &[&Symbol]) -> String {
    let mut s = String::new();
    s.push_str(path);
    s.push('\n');
    for sym in symbols {
        let container = sym
            .container
            .as_deref()
            .map(|c| format!(" ({c})"))
            .unwrap_or_default();
        s.push_str(&format!(
            "  {} {}{} L{}-{}\n",
            sym.kind, sym.name, container, sym.line_start, sym.line_end
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    fn sym(name: &str, kind: &str, ordinal: u32, line_start: u32, line_end: u32) -> Symbol {
        Symbol {
            ordinal,
            name: name.to_string(),
            kind: kind.to_string(),
            line_start,
            line_end,
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
    fn ranking_prefers_more_symbols_then_shallower_paths() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "many.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols(
                "hashA",
                "rust@1",
                &[
                    sym("a", "fn", 0, 1, 2),
                    sym("b", "fn", 1, 3, 4),
                    sym("c", "fn", 2, 5, 6),
                ],
            )
            .unwrap();
        store
            .upsert_file(repo_id, "few.rs", "hashB", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashB", "rust@1", &[sym("d", "fn", 0, 1, 2)])
            .unwrap();
        store
            .upsert_file(repo_id, "deep/nested/dir/few.rs", "hashC", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashC", "rust@1", &[sym("e", "fn", 0, 1, 2)])
            .unwrap();

        let out = build_map(&store, repo_id, "r", "", usize::MAX, 0).unwrap();
        let paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["many.rs", "few.rs", "deep/nested/dir/few.rs"],
            "more symbols first, then a shallower path beats a deeper one at equal symbol count: {paths:?}"
        );
        assert!(out.files[0].rank > out.files[1].rank);
        assert!(out.files[1].rank > out.files[2].rank);
    }

    #[test]
    fn frecency_can_promote_a_recently_opened_file() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "cold.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols(
                "hashA",
                "rust@1",
                &[sym("a", "fn", 0, 1, 2), sym("b", "fn", 1, 3, 4)],
            )
            .unwrap();
        store
            .upsert_file(repo_id, "hot.rs", "hashB", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashB", "rust@1", &[sym("c", "fn", 0, 1, 2)])
            .unwrap();

        let before = build_map(&store, repo_id, "r", "", usize::MAX, 0).unwrap();
        assert_eq!(before.files[0].path, "cold.rs", "2 symbols beats 1 alone");

        store.bump_file_open(repo_id, "hot.rs", 0).unwrap();
        let after = build_map(&store, repo_id, "r", "", usize::MAX, 0).unwrap();
        assert_eq!(
            after.files[0].path, "hot.rs",
            "a just-opened file's frecency boost must be able to overcome a small symbol-count deficit: {after:#?}"
        );
    }

    #[test]
    fn budget_fill_greedily_includes_whole_file_entries_and_flags_truncation() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        // Three files, each with one symbol whose name is long enough that
        // its rendered entry has a known, non-trivial token cost.
        for (path, name) in [("a.rs", "aaaa"), ("b.rs", "bbbb"), ("c.rs", "cccc")] {
            let hash = format!("hash_{path}");
            store.upsert_file(repo_id, path, &hash, "rust", 10).unwrap();
            store
                .replace_symbols(&hash, "rust@1", &[sym(name, "fn", 0, 1, 2)])
                .unwrap();
        }

        let unlimited = build_map(&store, repo_id, "r", "", usize::MAX, 0).unwrap();
        assert_eq!(unlimited.files.len(), 3);
        assert!(!unlimited.truncated);
        assert_eq!(unlimited.used_tokens, approx_tokens(&unlimited.outline));

        // Every entry has the SAME token cost (the three symbol names are
        // equal length) — a budget of exactly one entry's worth must admit
        // precisely the first-ranked file (`a.rs`, path-ascending tie-break)
        // and nothing more.
        let first_symbol = sym("aaaa", "fn", 0, 1, 2);
        let one_entry_tokens = approx_tokens(&render_file_entry("a.rs", &[&first_symbol]));
        let small = build_map(&store, repo_id, "r", "", one_entry_tokens, 0).unwrap();
        assert_eq!(
            small.files.len(),
            1,
            "exactly one entry should fit: {small:#?}"
        );
        assert_eq!(small.files[0].path, "a.rs");
        assert!(small.truncated);
        assert!(small.used_tokens <= small.budget);

        // The smallest possible budget still yields no panic and an honest
        // truncated flag when nothing fits at all.
        let tiny = build_map(&store, repo_id, "r", "", 1, 0).unwrap();
        assert!(tiny.files.is_empty());
        assert!(tiny.truncated);
        assert_eq!(tiny.used_tokens, 0);
    }

    #[test]
    fn path_scope_filters_to_the_given_directory_prefix() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        for path in ["src/foo/a.rs", "src/foo/b.rs", "src/foobar.rs", "other.rs"] {
            let hash = format!("hash_{path}");
            store.upsert_file(repo_id, path, &hash, "rust", 10).unwrap();
            store
                .replace_symbols(&hash, "rust@1", &[sym("x", "fn", 0, 1, 2)])
                .unwrap();
        }

        let out = build_map(&store, repo_id, "r", "src/foo", usize::MAX, 0).unwrap();
        let mut paths: Vec<&str> = out.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(
            paths,
            vec!["src/foo/a.rs", "src/foo/b.rs"],
            "must not match src/foobar.rs (component boundary, not a raw string prefix): {paths:?}"
        );
    }

    #[test]
    fn key_kind_symbols_are_excluded_from_the_map() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "config.yaml", "hashA", "yaml", 10)
            .unwrap();
        store
            .replace_symbols("hashA", "yaml@1", &[sym("top.nested", "key", 0, 1, 1)])
            .unwrap();

        let out = build_map(&store, repo_id, "r", "", usize::MAX, 0).unwrap();
        assert!(
            out.files.is_empty(),
            "a file whose only symbols are kind=key must not appear: {out:#?}"
        );
    }

    #[test]
    fn outline_text_matches_the_structured_files_list() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashA", "rust@1", &[sym("origin", "method", 0, 5, 7)])
            .unwrap();

        let out = build_map(&store, repo_id, "r", "", usize::MAX, 0).unwrap();
        assert!(out.outline.starts_with("a.rs\n"));
        assert!(out.outline.contains("method origin L5-7"));
    }
}
