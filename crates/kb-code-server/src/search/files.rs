//! Files lane (W2.1) — nucleo fuzzy match over the `files` table's paths,
//! the "jump to file" INSTANT search. Two entry points: [`FileIndex::
//! search`] (non-empty query — fuzzy rank, blended with frecency) and
//! [`FileIndex::recent`] (empty query — pure open-history recency, no
//! fuzzy pass at all; the route (`routes::search_files`) is what decides
//! which one a request gets).
//!
//! # In-memory path cache
//!
//! [`FileIndex`] holds one path list per repo, rebuilt lazily from
//! [`crate::store::Store::list_files`] whenever the store's generation
//! counter has moved past what the cached snapshot was built from (see
//! `store.rs`'s `generation` field doc). This is the "rebuild on a
//! generation counter" option the design brief offered as an alternative to
//! subscribing to the live-mirror `EventBus` directly — chosen because it
//! needs no background task wired into `bind_and_spawn` and no extra
//! shutdown-time cleanup, while still being correct: the next call after any
//! `mirror.updated`-triggering write sees the rebuilt list, because that
//! write is exactly what bumped the generation.
//!
//! # Frecency blend
//!
//! Each fuzzy candidate's raw nucleo score is blended with an ADDITIVE
//! recency boost:
//!
//! ```text
//! final_score = fuzzy_score + FRECENCY_BOOST_MAX * 0.5 ^ (age_secs / FRECENCY_HALF_LIFE_SECS)
//! ```
//!
//! where `age_secs` is the time since this path's most recent `/api/file`
//! read (`store::Store::last_opened_map`), or the boost is `0.0` if the
//! path has never been opened. The boost is exponential decay with a
//! [`FRECENCY_HALF_LIFE_SECS`]-second half-life (opened just now → full
//! boost; opened one half-life ago → half the boost; asymptotes to zero),
//! capped at [`FRECENCY_BOOST_MAX`] — small relative to a typical nucleo
//! match score (a short exact/prefix match commonly scores in the low
//! hundreds), so frecency can only break ties or near-ties between
//! similarly-good fuzzy matches; it can never promote a poor fuzzy match
//! over a clearly better one. `now_ms`/`opened_at` are both caller-supplied
//! (never read from the system clock here), so tests can pin the blend
//! deterministically.

use super::GenCached;
use crate::store::{Store, StoreError};
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Half-life (seconds) for the recency component of the frecency blend —
/// see the module doc's formula. Three days: a file opened moments ago
/// should visibly outrank an otherwise-tied fuzzy match; one opened weeks
/// ago should contribute almost nothing.
pub const FRECENCY_HALF_LIFE_SECS: f64 = 3.0 * 24.0 * 3600.0;

/// The maximum additive boost frecency can contribute — see the module doc.
pub const FRECENCY_BOOST_MAX: f64 = 60.0;

/// One ranked file hit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FileHit {
    pub repo: String,
    pub path: String,
    /// The blended score for a fuzzy hit (`search`), or the raw
    /// `opened_at` unix-ms timestamp for a recency hit (`recent`) — see
    /// each fn's doc. Not comparable across the two call sites; informational
    /// for the caller, not re-sorted by it.
    pub score: f64,
}

/// Per-daemon in-memory path cache + fuzzy search. One instance lives in
/// `AppState` (mirrors `store: Arc<Store>`'s own per-boot singleton shape) —
/// see the module doc.
#[derive(Default)]
pub struct FileIndex {
    cache: Mutex<HashMap<i64, GenCached<Vec<String>>>>,
    /// V70-A3X — the frecency blend's per-repo `last_opened_map` snapshot,
    /// gated on [`Store::opens_generation`] (NOT [`Store::generation`]):
    /// before this cache, [`Self::search`] ran a fresh `GROUP BY` over
    /// `file_opens` on EVERY call (every keystroke of an interactive
    /// search), even though open history changes far less often than that.
    /// A separate map (not folded into `cache` above) because it's keyed on
    /// a different generation counter entirely.
    recency_cache: Mutex<HashMap<i64, GenCached<HashMap<String, i64>>>>,
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self, store: &Store, repo_id: i64) -> Result<Arc<Vec<String>>, StoreError> {
        let current_gen = store.generation();
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = cache.get(&repo_id) {
                if entry.generation == current_gen {
                    return Ok(entry.value.clone());
                }
            }
        }
        let rows = store.list_files(repo_id)?;
        let paths: Arc<Vec<String>> = Arc::new(rows.into_iter().map(|r| r.path).collect());
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(
            repo_id,
            GenCached {
                generation: current_gen,
                value: paths.clone(),
            },
        );
        Ok(paths)
    }

    /// V70-A3X — [`Store::last_opened_map`], memoised per repo, gated on
    /// [`Store::opens_generation`] (see [`Self::recency_cache`]'s doc). Same
    /// lazy-rebuild shape as [`Self::snapshot`], just keyed on the OTHER
    /// counter.
    fn recency_snapshot(
        &self,
        store: &Store,
        repo_id: i64,
    ) -> Result<Arc<HashMap<String, i64>>, StoreError> {
        let current_gen = store.opens_generation();
        {
            let cache = self.recency_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = cache.get(&repo_id) {
                if entry.generation == current_gen {
                    return Ok(entry.value.clone());
                }
            }
        }
        let map: Arc<HashMap<String, i64>> = Arc::new(store.last_opened_map(repo_id)?);
        let mut cache = self.recency_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(
            repo_id,
            GenCached {
                generation: current_gen,
                value: map.clone(),
            },
        );
        Ok(map)
    }

    /// Fuzzy search `query` (must be non-empty — the caller, `routes::
    /// search_files`, is what routes an empty query to [`Self::recent`]
    /// instead) over `repos` (`(repo_name, repo_id)` pairs — pass every
    /// configured repo to search "all repos", or a single entry to scope to
    /// one), blended with open-history frecency, best-first, capped at
    /// `limit`. `now_ms` is the caller's clock (unix ms) for the frecency
    /// blend — see the module doc. `path_filter` (V70-A3X, `search::
    /// unified`'s `path:`/`repo:` PRE-filter) narrows the candidate set
    /// BEFORE fuzzy scoring/ranking when `Some` — a case-insensitive
    /// substring over the path, matching `search::grammar`'s `path:`
    /// semantics — so a match outside the top-scoring page can never be
    /// dropped by truncation before it's ever considered; `None` is the
    /// unfiltered, byte-identical-to-before path.
    pub fn search(
        &self,
        store: &Store,
        repos: &[(String, i64)],
        query: &str,
        limit: usize,
        now_ms: i64,
        path_filter: Option<&str>,
    ) -> Result<Vec<FileHit>, StoreError> {
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        // `Config::DEFAULT.match_paths()` narrows the boundary-bonus
        // delimiter set to the path separator — a match right after `/`
        // scores higher than one mid-segment, matching fzf-style file
        // pickers' ranking intuition.
        let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
        let mut buf = Vec::new();
        let mut hits: Vec<FileHit> = Vec::new();
        let path_filter_lower = path_filter.map(|s| s.to_lowercase());

        for (repo_name, repo_id) in repos {
            let paths = self.snapshot(store, *repo_id)?;
            if paths.is_empty() {
                continue;
            }
            let recency = self.recency_snapshot(store, *repo_id)?;
            for path in paths.iter() {
                if !super::path_prefilter_matches(path, path_filter_lower.as_deref()) {
                    continue;
                }
                buf.clear();
                let hay = Utf32Str::new(path, &mut buf);
                let Some(score) = pattern.score(hay, &mut matcher) else {
                    continue;
                };
                let boost = recency
                    .get(path)
                    .map(|&opened_at| recency_boost(now_ms, opened_at))
                    .unwrap_or(0.0);
                hits.push(FileHit {
                    repo: repo_name.clone(),
                    path: path.clone(),
                    score: score as f64 + boost,
                });
            }
        }

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.repo.cmp(&b.repo))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// Empty-query fallback: the `limit` most recently opened files across
    /// `repos`, newest first — no fuzzy pass at all (there is no needle to
    /// score against). `score` on each returned [`FileHit`] is the raw
    /// `opened_at` unix-ms timestamp (informational — recency IS the rank
    /// here, nothing further to blend). `path_filter` (V70-A3X) is the same
    /// `path:`/`repo:` PRE-filter [`Self::search`] takes — pushed into the
    /// underlying `recent_file_opens` SQL (not applied after `LIMIT`), so a
    /// matching-but-older file can't be pushed out of the recency window by
    /// non-matching, more-recent opens before the filter ever runs.
    pub fn recent(
        &self,
        store: &Store,
        repos: &[(String, i64)],
        limit: usize,
        path_filter: Option<&str>,
    ) -> Result<Vec<FileHit>, StoreError> {
        let repo_ids: Vec<i64> = repos.iter().map(|(_, id)| *id).collect();
        let rows = store.recent_file_opens(&repo_ids, limit, path_filter)?;
        Ok(rows
            .into_iter()
            .map(|(repo_id, path, opened_at)| {
                let repo_name = repos
                    .iter()
                    .find(|(_, id)| *id == repo_id)
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default();
                FileHit {
                    repo: repo_name,
                    path,
                    score: opened_at as f64,
                }
            })
            .collect())
    }
}

/// The additive frecency boost for a path last opened `opened_at_ms` (unix
/// ms), evaluated at `now_ms` — see the module doc's formula. Clamped at 0
/// age for an `opened_at` that is (implausibly) in the future, rather than
/// producing a boost above [`FRECENCY_BOOST_MAX`].
///
/// `pub(crate)` (not private) — `agentview::map`'s v1-rank reuses this
/// EXACT curve for its own frecency component, rather than growing a
/// second, possibly-drifting decay formula (see that module's doc).
pub(crate) fn recency_boost(now_ms: i64, opened_at_ms: i64) -> f64 {
    let age_secs = ((now_ms - opened_at_ms).max(0) as f64) / 1000.0;
    let factor = 0.5_f64.powf(age_secs / FRECENCY_HALF_LIFE_SECS);
    FRECENCY_BOOST_MAX * factor
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    fn seed_files(store: &Store, repo_id: i64, paths: &[&str]) {
        for (i, p) in paths.iter().enumerate() {
            store
                .upsert_file(repo_id, p, &format!("hash{i}"), "rust", 10)
                .unwrap();
        }
    }

    #[test]
    fn exact_prefix_and_scattered_matches_rank_sensibly() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(
            &store,
            repo_id,
            &[
                "src/lib.rs",                   // exact basename match
                "src/liberation.rs",            // contiguous prefix, longer overall
                "src/layout_indexer_bridge.rs", // "l-i-b" scattered far apart
            ],
        );
        let index = FileIndex::new();
        let hits = index
            .search(&store, &[("r".into(), repo_id)], "lib", 10, 0, None)
            .unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "src/lib.rs",
                "src/liberation.rs",
                "src/layout_indexer_bridge.rs",
            ],
            "expected exact > contiguous-prefix > scattered ranking, got {paths:?}"
        );
    }

    #[test]
    fn subsequence_fuzzy_match_finds_scattered_characters() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(&store, repo_id, &["src/git/repo.rs", "README.md"]);
        let index = FileIndex::new();
        // "gr" as a scattered subsequence of "git/repo.rs" (g...r).
        let hits = index
            .search(&store, &[("r".into(), repo_id)], "gr", 10, 0, None)
            .unwrap();
        assert!(hits.iter().any(|h| h.path == "src/git/repo.rs"));
        assert!(!hits.iter().any(|h| h.path == "README.md"));
    }

    #[test]
    fn no_match_yields_no_hits() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(&store, repo_id, &["src/lib.rs"]);
        let index = FileIndex::new();
        let hits = index
            .search(
                &store,
                &[("r".into(), repo_id)],
                "zzzzz-no-match",
                10,
                0,
                None,
            )
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn frecency_bump_changes_order_deterministically() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        // Both paths fuzzy-match "mod" equally well by construction (same
        // length, same match shape) so the frecency blend is what decides
        // the order.
        seed_files(&store, repo_id, &["a_mod.rs", "b_mod.rs"]);
        let index = FileIndex::new();

        let now_ms = 10 * FRECENCY_HALF_LIFE_SECS as i64 * 1000;
        // Before any open, order is a plain path tie-break (deterministic,
        // alphabetical via the sort's `then_with` on path).
        let before = index
            .search(&store, &[("r".into(), repo_id)], "mod", 10, now_ms, None)
            .unwrap();
        assert_eq!(before[0].path, "a_mod.rs");

        // Open "b_mod.rs" just now (age 0 -> full boost); "a_mod.rs" was
        // never opened (boost 0). "b_mod.rs" must now rank first.
        store.bump_file_open(repo_id, "b_mod.rs", now_ms).unwrap();
        let after = index
            .search(&store, &[("r".into(), repo_id)], "mod", 10, now_ms, None)
            .unwrap();
        assert_eq!(
            after[0].path, "b_mod.rs",
            "recently opened file should now lead: {after:?}"
        );

        // A stale open (ten half-lives ago) decays to ~0 boost and must not
        // out-rank a same-score-plus-recent-boost competitor.
        store.bump_file_open(repo_id, "a_mod.rs", 0).unwrap();
        let stale = index
            .search(&store, &[("r".into(), repo_id)], "mod", 10, now_ms, None)
            .unwrap();
        assert_eq!(stale[0].path, "b_mod.rs");
    }

    #[test]
    fn opening_a_file_does_not_bump_generation_but_the_recency_snapshot_still_updates() {
        // V70-A3X — the split-counter fix's own regression test: confirms
        // `bump_file_open` leaves `Store::generation()` untouched (so it can
        // never force-rebuild the WHOLE path cache) while the recency
        // snapshot `FileIndex::search` reads (gated on `opens_generation`)
        // still picks up the new open on the very next call.
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(&store, repo_id, &["a.rs", "b.rs"]);
        let index = FileIndex::new();

        // Warm both caches once.
        let _ = index
            .search(&store, &[("r".into(), repo_id)], "rs", 10, 0, None)
            .unwrap();
        let gen_before = store.generation();

        store.bump_file_open(repo_id, "b.rs", 5_000).unwrap();
        assert_eq!(
            store.generation(),
            gen_before,
            "a file open must not bump the main generation"
        );

        let now_ms = 5_000;
        let hits = index
            .search(&store, &[("r".into(), repo_id)], "rs", 10, now_ms, None)
            .unwrap();
        assert_eq!(
            hits[0].path, "b.rs",
            "the just-opened file must lead via its full recency boost — \
             the recency snapshot must have refreshed despite `generation` \
             not moving: {hits:?}"
        );
    }

    #[test]
    fn recency_boost_decays_with_age_and_is_bounded() {
        assert_eq!(
            recency_boost(1_000, 1_000),
            FRECENCY_BOOST_MAX,
            "zero age = full boost"
        );
        let half_life_ms = (FRECENCY_HALF_LIFE_SECS * 1000.0) as i64;
        let half = recency_boost(half_life_ms, 0);
        assert!(
            (half - FRECENCY_BOOST_MAX / 2.0).abs() < 0.01,
            "one half-life should halve the boost, got {half}"
        );
        // A future timestamp (clock skew) clamps to zero age, never a
        // boost above the max or a negative value.
        let future = recency_boost(0, 1_000_000);
        assert_eq!(future, FRECENCY_BOOST_MAX);
    }

    #[test]
    fn cache_rebuilds_after_a_generation_bump_and_serves_from_cache_otherwise() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(&store, repo_id, &["a.rs"]);
        let index = FileIndex::new();

        let hits = index
            .search(&store, &[("r".into(), repo_id)], "a", 10, 0, None)
            .unwrap();
        assert_eq!(hits.len(), 1);

        // A new file added after the first search must appear once the
        // generation has moved (no manual cache invalidation call needed).
        store
            .upsert_file(repo_id, "ab.rs", "hashNew", "rust", 1)
            .unwrap();
        let hits2 = index
            .search(&store, &[("r".into(), repo_id)], "a", 10, 0, None)
            .unwrap();
        assert_eq!(hits2.len(), 2);
    }

    #[test]
    fn empty_query_recent_returns_most_recently_opened_first() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(&store, repo_id, &["a.rs", "b.rs", "c.rs"]);
        store.bump_file_open(repo_id, "a.rs", 1_000).unwrap();
        store.bump_file_open(repo_id, "c.rs", 3_000).unwrap();
        store.bump_file_open(repo_id, "b.rs", 2_000).unwrap();

        let index = FileIndex::new();
        let hits = index
            .recent(&store, &[("r".into(), repo_id)], 10, None)
            .unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["c.rs", "b.rs", "a.rs"]);
    }

    #[test]
    fn recent_with_no_open_history_is_empty() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(&store, repo_id, &["a.rs"]);
        let index = FileIndex::new();
        let hits = index
            .recent(&store, &[("r".into(), repo_id)], 10, None)
            .unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn search_across_all_repos_tags_each_hit_with_its_own_repo_name() {
        let (_tmp, store) = open_store();
        let repo_a = store.upsert_repo("a", "/tmp/a").unwrap();
        let repo_b = store.upsert_repo("b", "/tmp/b").unwrap();
        seed_files(&store, repo_a, &["shared_name.rs"]);
        seed_files(&store, repo_b, &["shared_name.rs"]);
        let index = FileIndex::new();
        let hits = index
            .search(
                &store,
                &[("a".into(), repo_a), ("b".into(), repo_b)],
                "shared",
                10,
                0,
                None,
            )
            .unwrap();
        assert_eq!(hits.len(), 2);
        let repos: Vec<&str> = hits.iter().map(|h| h.repo.as_str()).collect();
        assert!(repos.contains(&"a"));
        assert!(repos.contains(&"b"));
    }

    // --- path_filter PRE-filter (V70-A3X) -----------------------------

    #[test]
    fn path_filter_is_applied_before_truncation_so_a_lower_scoring_match_survives() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        // Every decoy AND the target share the IDENTICAL suffix
        // "/needle.rs" behind a 4-character folder segment ("aaaN"/"keep"),
        // so the match span, boundary position, and overall path length are
        // structurally identical across every candidate — the fuzzy score
        // is GUARANTEED to tie (same pure function, same shape of input),
        // no dependency on nucleo's scoring internals. With a genuine tie,
        // the only remaining order is `path` ascending, so the
        // alphabetically-first decoys ("aaa0/.." < "keep/..") fill a tight
        // `limit` and push the target out — UNLESS `path_filter` pruned the
        // decoys before they ever entered the ranking.
        for i in 0..10 {
            let path = format!("aaa{i}/needle.rs");
            let hash = format!("hashDecoy{i}");
            store.upsert_file(repo_id, &path, &hash, "rust", 1).unwrap();
        }
        store
            .upsert_file(repo_id, "keep/needle.rs", "hashT", "rust", 1)
            .unwrap();

        let index = FileIndex::new();
        let unfiltered = index
            .search(&store, &[("r".into(), repo_id)], "needle", 3, 0, None)
            .unwrap();
        assert_eq!(unfiltered.len(), 3);
        assert!(
            !unfiltered.iter().any(|h| h.path == "keep/needle.rs"),
            "sanity check failed — decoys must fill the limit pre-filter: {unfiltered:?}"
        );

        let filtered = index
            .search(
                &store,
                &[("r".into(), repo_id)],
                "needle",
                3,
                0,
                Some("keep/"),
            )
            .unwrap();
        let paths: Vec<&str> = filtered.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["keep/needle.rs"], "got {paths:?}");
    }

    #[test]
    fn path_filter_is_case_insensitive_substring_like_the_grammar() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(&store, repo_id, &["src/Widget.rs", "tests/widget.rs"]);
        let index = FileIndex::new();
        let hits = index
            .search(
                &store,
                &[("r".into(), repo_id)],
                "widget",
                10,
                0,
                Some("SRC/"),
            )
            .unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["src/Widget.rs"], "got {paths:?}");
    }

    #[test]
    fn recent_path_filter_narrows_the_recency_window_not_just_the_page() {
        let (_tmp, store) = open_store();
        let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
        seed_files(
            &store,
            repo_id,
            &["keep/old.rs", "decoy_a.rs", "decoy_b.rs", "decoy_c.rs"],
        );
        // "keep/old.rs" was opened LONGEST ago; every decoy is more recent —
        // an unfiltered `limit=2` would push it out entirely.
        store.bump_file_open(repo_id, "keep/old.rs", 1_000).unwrap();
        store.bump_file_open(repo_id, "decoy_a.rs", 2_000).unwrap();
        store.bump_file_open(repo_id, "decoy_b.rs", 3_000).unwrap();
        store.bump_file_open(repo_id, "decoy_c.rs", 4_000).unwrap();

        let index = FileIndex::new();
        let unfiltered = index
            .recent(&store, &[("r".into(), repo_id)], 2, None)
            .unwrap();
        assert!(!unfiltered.iter().any(|h| h.path == "keep/old.rs"));

        let filtered = index
            .recent(&store, &[("r".into(), repo_id)], 2, Some("keep/"))
            .unwrap();
        let paths: Vec<&str> = filtered.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths, vec!["keep/old.rs"], "got {paths:?}");
    }
}
