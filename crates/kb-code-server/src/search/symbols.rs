//! Symbols lane (W2.1) — nucleo fuzzy match over symbol names, the "jump to
//! symbol" INSTANT search.
//!
//! # Haystack
//!
//! Each candidate's haystack is `"{container}::{name}"` when the symbol has
//! a container (e.g. `"GitRepo::open"`), or bare `name` otherwise — so a
//! query like `"gitrepo open"` (nucleo word-segments on whitespace) matches
//! a method by BOTH its enclosing type and its own name, not just the name
//! in isolation.
//!
//! # Current-files join
//!
//! Candidates come from [`crate::store::Store::symbols_for_repo`], which
//! joins `symbols` to `files` ON `blob_hash` — a symbol only surfaces here
//! if some path in the repo's CURRENT `files` rows still carries that blob
//! (ADR-2: symbols are blob-keyed, not path-keyed, so a since-deleted or
//! since-changed file's stale symbol rows are invisible to this join even
//! though the raw `symbols` table rows may still be on disk, uncollected).
//! Same in-memory cache shape as [`crate::search::files::FileIndex`] — see
//! that module's doc for the generation-counter rebuild rationale.
//!
//! # Ranking
//!
//! Primary key: nucleo fuzzy score, descending. Secondary key (ties only):
//! [`kind_priority`] — a definition kind like `fn`/`def` ranks above a
//! `method`, which ranks above a container type (`struct`/`class`/`trait`),
//! which ranks above everything else (see that fn's table) — the intuition
//! being that a bare-name query most often means "take me to the thing I'd
//! call", not the type it happens to live on. Final tie-break: name, then
//! path, for determinism.

use super::GenCached;
use crate::extract::Symbol;
use crate::store::{Store, StoreError};
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Results returned by one [`SymbolIndex::search`] call — a Wave-1
/// scan+rank, not a paginated surface (mirrors `routes::MAX_SYMBOL_MATCHES`,
/// the substring-scan cap this lane supersedes).
pub const MAX_RESULTS: usize = 200;

/// One ranked symbol hit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SymbolHit {
    pub repo: String,
    pub path: String,
    #[serde(flatten)]
    pub symbol: Symbol,
    pub score: u32,
}

/// `(path, symbol)` rows for one repo — `store::Store::symbols_for_repo`'s
/// own return shape, cached verbatim.
type SymbolRows = Vec<(String, Symbol)>;

#[derive(Default)]
pub struct SymbolIndex {
    cache: Mutex<HashMap<i64, GenCached<SymbolRows>>>,
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self, store: &Store, repo_id: i64) -> Result<Arc<SymbolRows>, StoreError> {
        let current_gen = store.generation();
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = cache.get(&repo_id) {
                if entry.generation == current_gen {
                    return Ok(entry.value.clone());
                }
            }
        }
        let rows: Arc<SymbolRows> = Arc::new(store.symbols_for_repo(repo_id)?);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(
            repo_id,
            GenCached {
                generation: current_gen,
                value: rows.clone(),
            },
        );
        Ok(rows)
    }

    /// Fuzzy search `query` (must be non-empty — enforced by the caller,
    /// `routes::search_symbols`, which 400s an empty query rather than
    /// returning every symbol unranked) over `repos`, best-first, capped at
    /// `min(limit, MAX_RESULTS)`. `path_filter` (V70-A3X, `search::
    /// unified`'s `path:`/`repo:` PRE-filter) restricts the candidate set to
    /// symbols whose FILE path matches (case-insensitive substring) BEFORE
    /// scoring/ranking/truncation — the same "narrow first, never
    /// post-filter a truncated page" contract [`super::files::FileIndex::
    /// search`] follows; `None` is the unfiltered, byte-identical-to-before
    /// path.
    pub fn search(
        &self,
        store: &Store,
        repos: &[(String, i64)],
        query: &str,
        limit: usize,
        path_filter: Option<&str>,
    ) -> Result<Vec<SymbolHit>, StoreError> {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        // Plain `Config::DEFAULT` (not `.match_paths()`): its default
        // delimiter set already includes `:` (`b"/,:;|"`), which is exactly
        // the boundary a `Container::name` haystack needs a bonus after —
        // `.match_paths()` would actually NARROW the delimiter set to just
        // the path separator and drop that bonus.
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf = Vec::new();
        let mut haystack = String::new();
        let mut hits: Vec<SymbolHit> = Vec::new();
        let path_filter_lower = path_filter.map(|s| s.to_lowercase());

        for (repo_name, repo_id) in repos {
            let rows = self.snapshot(store, *repo_id)?;
            for (path, symbol) in rows.iter() {
                if !super::path_prefilter_matches(path, path_filter_lower.as_deref()) {
                    continue;
                }
                haystack.clear();
                if let Some(container) = &symbol.container {
                    haystack.push_str(container);
                    haystack.push_str("::");
                    haystack.push_str(&symbol.name);
                } else {
                    haystack.push_str(&symbol.name);
                }
                buf.clear();
                let hay = Utf32Str::new(&haystack, &mut buf);
                let Some(score) = pattern.score(hay, &mut matcher) else {
                    continue;
                };
                hits.push(SymbolHit {
                    repo: repo_name.clone(),
                    path: path.clone(),
                    symbol: symbol.clone(),
                    score,
                });
            }
        }

        hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| kind_priority(&a.symbol.kind).cmp(&kind_priority(&b.symbol.kind)))
                .then_with(|| a.symbol.name.cmp(&b.symbol.name))
                .then_with(|| a.path.cmp(&b.path))
        });
        hits.truncate(limit.min(MAX_RESULTS));
        Ok(hits)
    }
}

/// Lower = ranked first among equal-score hits. Covers every kind
/// `extract.rs::map_kind` currently produces, plus a defensive default
/// (`5`) for anything it doesn't — e.g. a hypothetical future `"field"`
/// kind, forward-compatible without a code change here.
///
/// | priority | kinds |
/// |---|---|
/// | 0 | `fn`, `def` |
/// | 1 | `method`, `singleton_method` |
/// | 2 | `struct`, `class`, `enum`, `trait`, `singleton_class` |
/// | 3 | `type_alias`, `mod`, `module`, `macro`, `alias`, `union` |
/// | 4 | `const` |
/// | 5 | everything else |
fn kind_priority(kind: &str) -> u8 {
    match kind {
        "fn" | "def" => 0,
        "method" | "singleton_method" => 1,
        "struct" | "class" | "enum" | "trait" | "singleton_class" => 2,
        "type_alias" | "mod" | "module" | "macro" | "alias" | "union" => 3,
        "const" => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    /// Builds `Symbol`s with DISTINCT, sequential ordinals — the `symbols`
    /// table's primary key is `(blob_hash, salt, ordinal)`, so a test
    /// seeding more than one symbol per blob under the same ordinal (e.g.
    /// every symbol hard-coded to `ordinal: 0`) would violate that
    /// constraint on the second `INSERT` and panic. `sym1` covers the
    /// (common) single-symbol-per-blob case without the array-literal
    /// ceremony.
    fn syms(specs: &[(&str, &str, Option<&str>)]) -> Vec<Symbol> {
        specs
            .iter()
            .enumerate()
            .map(|(ordinal, (name, kind, container))| Symbol {
                ordinal: ordinal as u32,
                name: name.to_string(),
                kind: kind.to_string(),
                line_start: 1,
                line_end: 1,
                col_start: 0,
                col_end: 1,
                container: container.map(|c| c.to_string()),
                signature: None,
                doc: None,
                param_min: None,
                param_max: None,
            })
            .collect()
    }

    fn sym1(name: &str, kind: &str, container: Option<&str>) -> Vec<Symbol> {
        syms(&[(name, kind, container)])
    }

    #[test]
    fn fuzzy_matches_bare_name_and_container_qualified_name() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "git/mod.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols(
                "hashA",
                "rust@1",
                &syms(&[("open", "method", Some("GitRepo")), ("close", "fn", None)]),
            )
            .unwrap();

        let index = SymbolIndex::new();
        let hits = index
            .search(&store, &[("r".into(), repo_id)], "GitRepo::open", 10, None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].symbol.name, "open");

        // Bare "open" also matches (container is optional context, not a
        // required prefix).
        let bare = index
            .search(&store, &[("r".into(), repo_id)], "open", 10, None)
            .unwrap();
        assert!(bare.iter().any(|h| h.symbol.name == "open"));
    }

    #[test]
    fn ranking_sanity_exact_beats_prefix_beats_scattered() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols(
                "hashA",
                "rust@1",
                &syms(&[
                    ("run", "fn", None),
                    ("runner", "fn", None),
                    ("re_use_naming", "fn", None), // "r-u-n" scattered
                ]),
            )
            .unwrap();
        let index = SymbolIndex::new();
        let hits = index
            .search(&store, &[("r".into(), repo_id)], "run", 10, None)
            .unwrap();
        let names: Vec<&str> = hits.iter().map(|h| h.symbol.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["run", "runner", "re_use_naming"],
            "got {names:?}"
        );
    }

    #[test]
    fn kind_priority_breaks_ties_fn_over_struct() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        // Two symbols with the IDENTICAL name (so nucleo assigns them the
        // identical score for the same query) but different kinds — the
        // kind-priority tie-break is the only thing that can order them.
        store
            .replace_symbols(
                "hashA",
                "rust@1",
                &syms(&[("Widget", "struct", None), ("Widget", "fn", None)]),
            )
            .unwrap();
        let index = SymbolIndex::new();
        let hits = index
            .search(&store, &[("r".into(), repo_id)], "Widget", 10, None)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].score, hits[1].score, "must be a genuine score tie");
        assert_eq!(
            hits[0].symbol.kind, "fn",
            "fn must rank before struct on a tie"
        );
        assert_eq!(hits[1].symbol.kind, "struct");
    }

    #[test]
    fn symbol_join_drops_stale_blobs() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashOld", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashOld", "rust@1", &sym1("gone_symbol", "fn", None))
            .unwrap();

        let index = SymbolIndex::new();
        let before = index
            .search(&store, &[("r".into(), repo_id)], "gone_symbol", 10, None)
            .unwrap();
        assert_eq!(before.len(), 1);

        // The file is edited: same path, new blob_hash, no symbols indexed
        // for it yet. The OLD blob's symbol row is still physically present
        // in `symbols` (ADR-2 never prunes it), but no CURRENT `files` row
        // points at `hashOld` any more, so it must vanish from the join.
        store
            .upsert_file(repo_id, "a.rs", "hashNew", "rust", 12)
            .unwrap();
        let after = index
            .search(&store, &[("r".into(), repo_id)], "gone_symbol", 10, None)
            .unwrap();
        assert!(
            after.is_empty(),
            "stale-blob symbol must not surface: {after:?}"
        );
    }

    #[test]
    fn results_are_capped_at_max_results() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        let names: Vec<String> = (0..(MAX_RESULTS + 50))
            .map(|i| format!("item_{i}"))
            .collect();
        let specs: Vec<(&str, &str, Option<&str>)> =
            names.iter().map(|n| (n.as_str(), "fn", None)).collect();
        let many = syms(&specs);
        store.replace_symbols("hashA", "rust@1", &many).unwrap();

        let index = SymbolIndex::new();
        let hits = index
            .search(&store, &[("r".into(), repo_id)], "item", 10_000, None)
            .unwrap();
        assert_eq!(hits.len(), MAX_RESULTS);
    }

    #[test]
    fn no_match_yields_no_hits() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        store
            .upsert_file(repo_id, "a.rs", "hashA", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashA", "rust@1", &sym1("run", "fn", None))
            .unwrap();
        let index = SymbolIndex::new();
        let hits = index
            .search(&store, &[("r".into(), repo_id)], "zzzz-nope", 10, None)
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn path_filter_is_applied_before_truncation_so_a_lower_scoring_match_survives() {
        // V70-A3X — every decoy AND the target declare a symbol with the
        // IDENTICAL bare name "run" (no container), so every candidate's
        // haystack is the literal string "run" and every score is
        // GUARANTEED identical (same pure function, same input) — no
        // dependency on nucleo's path-scoring internals. With a genuine
        // score/kind/name three-way tie, the only remaining tie-break is
        // `path` ascending, so the alphabetically-first decoys fill a tight
        // `limit` and push "keep/target.rs" out — UNLESS `path_filter`
        // pruned the decoys before they ever entered the ranking.
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        for i in 0..10 {
            let path = format!("aaa{i}.rs");
            let hash = format!("hashDecoy{i}");
            store
                .upsert_file(repo_id, &path, &hash, "rust", 10)
                .unwrap();
            store
                .replace_symbols(&hash, "rust@1", &sym1("run", "fn", None))
                .unwrap();
        }
        store
            .upsert_file(repo_id, "keep/target.rs", "hashK", "rust", 10)
            .unwrap();
        store
            .replace_symbols("hashK", "rust@1", &sym1("run", "fn", None))
            .unwrap();

        let index = SymbolIndex::new();
        let unfiltered = index
            .search(&store, &[("r".into(), repo_id)], "run", 3, None)
            .unwrap();
        assert_eq!(unfiltered.len(), 3);
        assert!(
            !unfiltered.iter().any(|h| h.path == "keep/target.rs"),
            "sanity check failed — decoys must fill the limit pre-filter: {unfiltered:?}"
        );

        let filtered = index
            .search(&store, &[("r".into(), repo_id)], "run", 3, Some("keep/"))
            .unwrap();
        assert_eq!(filtered.len(), 1, "got {filtered:?}");
        assert_eq!(filtered[0].path, "keep/target.rs");
    }
}
