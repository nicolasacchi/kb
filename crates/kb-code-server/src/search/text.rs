//! Text lane (W2.1) — `grep-searcher` STREAMING literal/regex search over a
//! repo's WORKING-TREE files (not the git ODB — deliberately: a search that
//! only ever saw the last commit would miss whatever an operator or agent
//! is actively editing).
//!
//! # Walk list
//!
//! Candidate paths come from [`crate::store::Store::list_files`] — the SAME
//! table the files/symbols lanes read, kept current by the live-mirror sink
//! (`sink.rs`) — never a fresh filesystem walk. This means the text lane
//! shares the ingest caps: a path whose `files.lang` is [`TIER_TOO_LARGE`],
//! [`TIER_BINARY`], or [`TIER_LFS`] is skipped before it's ever opened.
//! `TIER_UNKNOWN` (no tree-sitter grammar — e.g. `README.md`, `Cargo.toml`)
//! is deliberately NOT skipped: text search has no notion of "unsupported
//! language", only "not textual content".
//!
//! # Match extraction
//!
//! `grep-searcher` reports one [`grep_searcher::SinkMatch`] per matching
//! LINE (its own line-oriented convention — never per occurrence within a
//! line). [`CollectSink::matched`] additionally re-runs the same
//! [`grep_matcher::Matcher::find`] on that line's own bytes to recover the
//! byte range of the match WITHIN the line (`SinkMatch` only exposes the
//! line's absolute offset in the file, not the sub-range inside it) — this
//! is what a caller needs to highlight the hit.
//!
//! # Caps
//!
//! Two independent stop conditions, surfaced as two independent flags so a
//! caller can tell them apart: [`MAX_TOTAL_MATCHES`]/[`MAX_MATCHES_PER_FILE`]
//! (a hard result-count cap → [`TextSearchResponse::truncated`]) and
//! `time_budget` (a wall-clock soft-stop, checked before scanning each file
//! AND inside the per-line callback → [`TextSearchResponse::
//! time_budget_exceeded`]). Either can fire independently; a response can
//! set both.

use crate::ingest::{TIER_BINARY, TIER_LFS, TIER_TOO_LARGE};
use crate::store::{Store, StoreError};
use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch};
use std::path::Path;
use std::time::{Duration, Instant};

/// Hard cap on total matches (summed across every file) returned by one
/// [`search_text`] call.
pub const MAX_TOTAL_MATCHES: usize = 500;
/// Hard cap on matches returned per FILE — keeps one match-dense file (e.g.
/// a generated data file) from consuming the whole [`MAX_TOTAL_MATCHES`]
/// budget and starving every other file's results.
pub const MAX_MATCHES_PER_FILE: usize = 50;
/// Default wall-clock soft-stop — see the module doc.
pub const DEFAULT_TIME_BUDGET: Duration = Duration::from_millis(300);

const SKIP_TIERS: [&str; 3] = [TIER_TOO_LARGE, TIER_BINARY, TIER_LFS];

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TextMatch {
    pub line_no: u64,
    /// The matched line's text, with its trailing line terminator(s)
    /// stripped. Lossy-decoded (`String::from_utf8_lossy`) — the file
    /// itself is already known-UTF8 by construction (`TIER_BINARY` files
    /// are skipped before this point), so this is a defensive fallback, not
    /// an expected path.
    pub line: String,
    /// `(start, end)` byte offsets of the match WITHIN `line` (not the
    /// file) — see the module doc's "Match extraction" section.
    pub byte_range: (usize, usize),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TextFileResult {
    pub path: String,
    pub matches: Vec<TextMatch>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TextSearchResponse {
    pub results: Vec<TextFileResult>,
    /// Hit [`MAX_TOTAL_MATCHES`]/[`MAX_MATCHES_PER_FILE`] — more matches
    /// exist but were not returned.
    pub truncated: bool,
    /// The `time_budget` soft-stop tripped — the walk stopped early and
    /// unscanned files may still contain matches.
    pub time_budget_exceeded: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum TextSearchError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("invalid search pattern: {0}")]
    Pattern(String),
    #[error("query must not be empty")]
    EmptyQuery,
}

pub type Result<T> = std::result::Result<T, TextSearchError>;

/// Streaming literal/regex search over `repo_root`'s working-tree files,
/// scoped to `repo_id`'s current `files` rows (see the module doc). `regex`
/// selects literal (`false`) vs. regex (`true`) syntax; `case_sensitive`
/// selects case-sensitive vs. case-insensitive matching. `time_budget` is
/// the wall-clock soft-stop (pass [`DEFAULT_TIME_BUDGET`] unless a caller
/// has a reason to override it — tests use a zero budget to pin the
/// soft-stop deterministically). `path_filter` (V70-A3X, `search::
/// unified`'s `path:`/`repo:` PRE-filter) is an optional case-insensitive
/// substring over the path — a non-matching file is skipped BEFORE the
/// (real, per-file) I/O of opening and scanning it, so the walk is both
/// narrower AND faster with a filter applied, and the 300ms
/// [`DEFAULT_TIME_BUDGET`] stretches further over the files that actually
/// matter, rather than being spent scanning files the caller will discard.
#[allow(clippy::too_many_arguments)]
pub fn search_text(
    store: &Store,
    repo_root: &Path,
    repo_id: i64,
    query: &str,
    regex: bool,
    case_sensitive: bool,
    time_budget: Duration,
    path_filter: Option<&str>,
) -> Result<TextSearchResponse> {
    if query.is_empty() {
        return Err(TextSearchError::EmptyQuery);
    }
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        .fixed_strings(!regex)
        .build(query)
        .map_err(|e| TextSearchError::Pattern(e.to_string()))?;

    let files = store.list_files(repo_id)?;
    let deadline = Instant::now() + time_budget;
    let path_filter_lower = path_filter.map(|s| s.to_lowercase());

    let mut results = Vec::new();
    let mut total_matches = 0usize;
    let mut truncated = false;
    let mut time_budget_exceeded = false;
    let mut searcher: Searcher = SearcherBuilder::new().line_number(true).build();

    for file in files {
        if SKIP_TIERS.contains(&file.lang.as_str()) {
            continue;
        }
        if !super::path_prefilter_matches(&file.path, path_filter_lower.as_deref()) {
            continue;
        }
        if Instant::now() >= deadline {
            time_budget_exceeded = true;
            break;
        }
        if total_matches >= MAX_TOTAL_MATCHES {
            truncated = true;
            break;
        }

        let abs = repo_root.join(&file.path);
        let total_remaining = MAX_TOTAL_MATCHES - total_matches;
        let mut sink = CollectSink {
            matcher: &matcher,
            matches: Vec::new(),
            per_file_cap: MAX_MATCHES_PER_FILE,
            total_remaining,
            deadline,
            hit_deadline: false,
            hit_cap: false,
        };
        // A vanished/unreadable file (deleted since the last index, or a
        // path kind `list_files` doesn't expect) is skipped, not fatal —
        // the caller gets a best-effort result over whatever WAS readable,
        // mirroring `sink.rs`'s own tolerate-and-continue posture for a
        // single bad path.
        if searcher.search_path(&matcher, &abs, &mut sink).is_err() {
            continue;
        }

        if sink.hit_deadline {
            time_budget_exceeded = true;
        }
        if sink.hit_cap {
            truncated = true;
        }
        total_matches += sink.matches.len();
        if !sink.matches.is_empty() {
            results.push(TextFileResult {
                path: file.path,
                matches: sink.matches,
            });
        }
        if total_matches >= MAX_TOTAL_MATCHES {
            truncated = true;
        }
        if time_budget_exceeded {
            break;
        }
    }

    Ok(TextSearchResponse {
        results,
        truncated,
        time_budget_exceeded,
    })
}

struct CollectSink<'a> {
    matcher: &'a RegexMatcher,
    matches: Vec<TextMatch>,
    per_file_cap: usize,
    total_remaining: usize,
    deadline: Instant,
    hit_deadline: bool,
    /// Set when this file's own [`Self::matches`] stopped early because
    /// `per_file_cap` or `total_remaining` was reached — i.e. this FILE has
    /// more matches than were returned, independent of whether the overall
    /// [`MAX_TOTAL_MATCHES`] cap was also reached (`search_text` ORs this
    /// into the response's `truncated` flag).
    hit_cap: bool,
}

impl Sink for CollectSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> std::result::Result<bool, Self::Error> {
        if Instant::now() >= self.deadline {
            self.hit_deadline = true;
            return Ok(false);
        }
        let raw = mat.bytes();
        let line = String::from_utf8_lossy(raw)
            .trim_end_matches(['\n', '\r'])
            .to_string();
        // `RegexMatcher`'s `Matcher::Error` is `grep_matcher::NoError` (a
        // proof-of-no-error type — see grep-regex's own `impl Matcher for
        // RegexMatcher`), so this can never actually be `Err`; a missing
        // sub-match (defensive only — the line matched, so the pattern DID
        // match somewhere in it) falls back to an empty `(0, 0)` range
        // rather than panicking. Clamped to `line.len()` (the TRIMMED
        // string, shorter than `raw` by the terminator's byte length) —
        // defensive against a pathological pattern that matches into the
        // terminator itself, which would otherwise make `byte_range` an
        // out-of-bounds slice into `line` for a caller that indexes it.
        let byte_range = self
            .matcher
            .find(raw)
            .unwrap_or(None)
            .map(|m| (m.start().min(line.len()), m.end().min(line.len())))
            .unwrap_or((0, 0));
        self.matches.push(TextMatch {
            line_no: mat.line_number().unwrap_or(0),
            line,
            byte_range,
        });
        if self.matches.len() >= self.per_file_cap || self.matches.len() >= self.total_remaining {
            self.hit_cap = true;
            return Ok(false);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest;
    use std::fs;

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    /// Seeds a `files` row AND writes the matching bytes to the real
    /// working-tree path under `repo_root` — `search_text` reads bytes off
    /// disk, so the store row alone isn't enough.
    fn seed(store: &Store, repo_root: &Path, repo_id: i64, path: &str, lang: &str, content: &[u8]) {
        let abs = repo_root.join(path);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&abs, content).unwrap();
        let blob_hash = ingest::git_blob_hash(content);
        store
            .upsert_file(repo_id, path, &blob_hash, lang, content.len() as u64)
            .unwrap();
    }

    #[test]
    fn literal_search_finds_and_groups_matches_per_file() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(
            &store,
            root.path(),
            repo_id,
            "a.rs",
            "rust",
            b"fn add(a: i32) -> i32 {\n    a + 1\n}\nfn add_two() {}\n",
        );
        seed(
            &store,
            root.path(),
            repo_id,
            "b.rs",
            "rust",
            b"fn sub() {}\n",
        );

        let resp = search_text(
            &store,
            root.path(),
            repo_id,
            "fn add",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert_eq!(
            resp.results.len(),
            1,
            "only a.rs contains \"fn add\": {resp:?}"
        );
        assert_eq!(resp.results[0].path, "a.rs");
        assert_eq!(resp.results[0].matches.len(), 2, "two matching lines");
        assert_eq!(resp.results[0].matches[0].line_no, 1);
        assert!(resp.results[0].matches[0].line.contains("fn add(a"));
        let (s, e) = resp.results[0].matches[0].byte_range;
        assert_eq!(&resp.results[0].matches[0].line[s..e], "fn add");
        assert!(!resp.truncated);
        assert!(!resp.time_budget_exceeded);
    }

    #[test]
    fn literal_mode_does_not_interpret_regex_metacharacters() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        // "a.b" as a REGEX (`.` = any char) matches BOTH lines; as a LITERAL
        // it must match only the line with a genuine dot.
        seed(
            &store,
            root.path(),
            repo_id,
            "a.txt",
            "unknown",
            b"a.b\naxb\n",
        );

        let literal = search_text(
            &store,
            root.path(),
            repo_id,
            "a.b",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert_eq!(
            literal.results.len(),
            1,
            "literal mode must not treat '.' as a wildcard: {literal:?}"
        );
        assert_eq!(literal.results[0].matches.len(), 1);
        assert_eq!(literal.results[0].matches[0].line, "a.b");

        let as_regex = search_text(
            &store,
            root.path(),
            repo_id,
            "a.b",
            true,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert_eq!(
            as_regex.results[0].matches.len(),
            2,
            "regex mode's '.' must match any char: {as_regex:?}"
        );
    }

    #[test]
    fn regex_mode_interprets_metacharacters() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(
            &store,
            root.path(),
            repo_id,
            "a.rs",
            "rust",
            b"fn add() {}\nfn sub() {}\nfn mul() {}\n",
        );
        let resp = search_text(
            &store,
            root.path(),
            repo_id,
            "^fn (add|mul)",
            true,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].matches.len(), 2);
        assert_eq!(resp.results[0].matches[0].line, "fn add() {}");
        assert_eq!(resp.results[0].matches[1].line, "fn mul() {}");
    }

    #[test]
    fn case_sensitivity_is_respected() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(
            &store,
            root.path(),
            repo_id,
            "a.rs",
            "rust",
            b"struct Widget;\n",
        );

        let sensitive = search_text(
            &store,
            root.path(),
            repo_id,
            "widget",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert!(
            sensitive.results.is_empty(),
            "case-sensitive must not match differing case"
        );

        let insensitive = search_text(
            &store,
            root.path(),
            repo_id,
            "widget",
            false,
            false,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert_eq!(insensitive.results.len(), 1);
    }

    #[test]
    fn skips_binary_too_large_and_lfs_tiers() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(
            &store,
            root.path(),
            repo_id,
            "ok.rs",
            "rust",
            b"needle here\n",
        );
        seed(
            &store,
            root.path(),
            repo_id,
            "big.bin",
            ingest::TIER_TOO_LARGE,
            b"needle here\n",
        );
        seed(
            &store,
            root.path(),
            repo_id,
            "bin.dat",
            ingest::TIER_BINARY,
            b"needle here\n",
        );
        seed(
            &store,
            root.path(),
            repo_id,
            "asset.lfs",
            ingest::TIER_LFS,
            b"needle here\n",
        );
        // TIER_UNKNOWN (no grammar) must still be searched — text search
        // has no "unsupported language" concept.
        seed(
            &store,
            root.path(),
            repo_id,
            "README.md",
            ingest::TIER_UNKNOWN,
            b"needle here\n",
        );

        let resp = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        let paths: Vec<&str> = resp.results.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths.len(), 2, "got {paths:?}");
        assert!(paths.contains(&"ok.rs"));
        assert!(paths.contains(&"README.md"));
    }

    #[test]
    fn per_file_and_total_caps_set_the_truncated_flag() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        let mut content = String::new();
        for _ in 0..(MAX_MATCHES_PER_FILE + 20) {
            content.push_str("needle\n");
        }
        seed(
            &store,
            root.path(),
            repo_id,
            "dense.txt",
            "unknown",
            content.as_bytes(),
        );

        let resp = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].matches.len(), MAX_MATCHES_PER_FILE);
        assert!(resp.truncated);
    }

    #[test]
    fn zero_time_budget_trips_the_soft_stop_flag() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(&store, root.path(), repo_id, "a.rs", "rust", b"needle\n");

        let resp = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            Duration::ZERO,
            None,
        )
        .unwrap();
        assert!(
            resp.time_budget_exceeded,
            "a zero budget must trip immediately: {resp:?}"
        );
        assert!(resp.results.is_empty());
    }

    #[test]
    fn empty_query_is_rejected() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        let err = search_text(
            &store,
            root.path(),
            repo_id,
            "",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, TextSearchError::EmptyQuery));
    }

    #[test]
    fn a_vanished_file_is_skipped_not_fatal() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(&store, root.path(), repo_id, "gone.rs", "rust", b"needle\n");
        seed(&store, root.path(), repo_id, "here.rs", "rust", b"needle\n");
        // The store still has a row for "gone.rs", but the file on disk is
        // removed out from under it (a race the live mirror hasn't caught
        // up to yet).
        fs::remove_file(root.path().join("gone.rs")).unwrap();

        let resp = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].path, "here.rs");
    }

    #[test]
    fn path_filter_narrows_the_walk_not_just_the_results() {
        // V70-A3X — a file OUTSIDE the filter that would otherwise consume
        // the entire per-file match cap must not affect a file INSIDE the
        // filter: proves the filter skips the file before it's ever opened
        // and scanned, not merely dropped from the response afterward.
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(
            &store,
            root.path(),
            repo_id,
            "keep/a.rs",
            "rust",
            b"needle\n",
        );
        seed(
            &store,
            root.path(),
            repo_id,
            "decoy/b.rs",
            "rust",
            b"needle\n",
        );

        let unfiltered = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            None,
        )
        .unwrap();
        assert_eq!(unfiltered.results.len(), 2, "got {unfiltered:?}");

        let filtered = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            Some("keep/"),
        )
        .unwrap();
        assert_eq!(filtered.results.len(), 1, "got {filtered:?}");
        assert_eq!(filtered.results[0].path, "keep/a.rs");
    }

    #[test]
    fn path_filter_is_case_insensitive_substring_like_the_grammar() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(
            &store,
            root.path(),
            repo_id,
            "Src/widget.rs",
            "rust",
            b"needle\n",
        );
        seed(
            &store,
            root.path(),
            repo_id,
            "tests/widget.rs",
            "rust",
            b"needle\n",
        );

        let resp = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            Some("src/"),
        )
        .unwrap();
        assert_eq!(resp.results.len(), 1, "got {resp:?}");
        assert_eq!(resp.results[0].path, "Src/widget.rs");
    }
}
