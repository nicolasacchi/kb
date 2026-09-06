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
//! set both. [`TextSearchResponse::scanned`]/[`TextSearchResponse::total`]
//! (V71-D1b) are the honest accounting behind those flags — "how many of the
//! eligible files did we actually get to open" versus "how many were
//! eligible in the first place" — so a caller (or a human reading a bug
//! report) never has to infer a budget miss from an empty `results` array
//! alone. Both are additive, always-present fields: absent from every
//! response before this unit, and present on every one after it.
//!
//! # Scan order (V71-D1b)
//!
//! `files` used to be scanned in [`Store::list_files`]'s own `ORDER BY
//! path` — alphabetical, i.e. not a relevance signal, and on a large repo
//! (6,553 files measured) the tail of the alphabet routinely never got
//! opened before [`DEFAULT_TIME_BUDGET`] tripped: a query whose answer was
//! one exact line in `app/models/fiscal_entry.rb` returned
//! `time_budget_exceeded: true` with ZERO results, because `search_text`
//! never got past `app/`. Ranking cannot fix this — it runs AFTER the scan
//! (`rank_by_rarity`, below, only reorders what was actually found).
//!
//! [`reorder_candidates_first`] moves the files most likely to matter to the
//! FRONT of the walk, before the budget/cap loop ever starts, from two
//! signals: (a) the query's own [`super::matcher::identifier_atoms`] tested
//! as a case-insensitive substring against each file's path (free — no
//! store read, just the path list already in hand), and (b) an optional
//! caller-supplied [`super::LaneOpts::candidate_paths`] set — in practice,
//! `search::unified`'s text-lane runner passing the paths of every symbol
//! (from a WARM `search::symbols::SymbolIndex` snapshot, never a cold
//! rebuild — see that type's `cached_snapshot_if_warm` doc for why this
//! must never pay `symbols_for_repo`'s full-table cost inline) whose name
//! contains one of those same atoms. Together this is the same "path/
//! basename/symbol name" candidate shape `path:` (V70-A3X) already proved
//! for an explicit filter, generalised into an implicit scan-order hint
//! that applies even when the caller wrote no filter at all. A file that is
//! not a candidate is never skipped — only scanned LATER, after every
//! candidate — so a query with no token overlap anywhere (a plain content
//! grep for a string that names nothing) degrades exactly to the old
//! alphabetical order, byte-for-byte.

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
    /// V71-D1b — how many of [`Self::total`] eligible files were actually
    /// opened and scanned before the walk stopped (for any reason: the
    /// budget, the match cap, or simply running out of files). Additive —
    /// see the module doc's "Caps" section.
    pub scanned: usize,
    /// V71-D1b — how many files were ELIGIBLE for this search: `files`
    /// rows for this repo whose tier isn't skipped ([`SKIP_TIERS`]) and
    /// that pass the `path:`/`repo:` pre-filter, if any — i.e. the walk
    /// list's length before the budget/cap loop runs. `scanned == total`
    /// with `results` empty is a genuine zero-hit answer; `scanned < total`
    /// with `results` empty (always paired with `truncated` or
    /// `time_budget_exceeded`) means the walk stopped before it could say
    /// that honestly — never presented as a bare empty array with no
    /// explanation.
    pub total: usize,
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
    opts: &super::LaneOpts,
) -> Result<TextSearchResponse> {
    if query.is_empty() {
        return Err(TextSearchError::EmptyQuery);
    }
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        .fixed_strings(!regex)
        .build(query)
        .map_err(|e| TextSearchError::Pattern(e.to_string()))?;

    let path_filter_lower = opts.path_filter.map(|s| s.to_lowercase());
    // V71-D1b — tier-skip and the `path:`/`repo:` pre-filter narrow the walk
    // list ONCE, up front, so `total`/`scanned` (below) count the same
    // eligible set the reorder and the budget loop both see — not the raw
    // `files` table row count, which would make `total` overstate what this
    // query could ever have matched.
    let mut files: Vec<_> = store
        .list_files(repo_id)?
        .into_iter()
        .filter(|f| !SKIP_TIERS.contains(&f.lang.as_str()))
        .filter(|f| super::path_prefilter_matches(&f.path, path_filter_lower.as_deref()))
        .collect();
    let total = files.len();
    reorder_candidates_first(&mut files, query, opts.candidate_paths);

    let deadline = Instant::now() + time_budget;
    let mut results = Vec::new();
    let mut total_matches = 0usize;
    let mut truncated = false;
    let mut time_budget_exceeded = false;
    let mut scanned = 0usize;
    let mut searcher: Searcher = SearcherBuilder::new().line_number(true).build();

    for file in files {
        if Instant::now() >= deadline {
            time_budget_exceeded = true;
            break;
        }
        if total_matches >= MAX_TOTAL_MATCHES {
            truncated = true;
            break;
        }
        scanned += 1;

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

    // V71-D1 — the lexical lane's only ranking. Off, results stay in
    // `list_files`' `ORDER BY path`, which is what shipped.
    if opts.factors.lexical_rarity {
        rank_by_rarity(&mut results);
    }

    Ok(TextSearchResponse {
        results,
        truncated,
        time_budget_exceeded,
        scanned,
        total,
    })
}

/// V71-D1b — move likely-relevant files to the FRONT of the walk — see the
/// module doc's "Scan order" section for the full rationale. A file is a
/// candidate if its path contains (case-insensitively) one of `query`'s
/// [`super::matcher::identifier_atoms`], OR its path is a member of the
/// caller-supplied `candidate_paths` (symbol-name matches, when the caller
/// has a warm snapshot to offer). Stable: within each of the two groups,
/// files keep the order they arrived in (`Store::list_files`'s `ORDER BY
/// path`), so a query with no signal at all reorders nothing.
fn reorder_candidates_first(
    files: &mut Vec<crate::store::FileRow>,
    query: &str,
    candidate_paths: Option<&std::collections::HashSet<String>>,
) {
    let atoms = super::matcher::identifier_atoms(query);
    if atoms.is_empty() && candidate_paths.is_none_or(|set| set.is_empty()) {
        return;
    }
    let is_candidate = |f: &crate::store::FileRow| -> bool {
        if candidate_paths.is_some_and(|set| set.contains(&f.path)) {
            return true;
        }
        if atoms.is_empty() {
            return false;
        }
        let lower = f.path.to_lowercase();
        atoms.iter().any(|a| lower.contains(a.as_str()))
    };
    let owned = std::mem::take(files);
    let (mut candidates, mut rest): (Vec<_>, Vec<_>) = owned.into_iter().partition(is_candidate);
    candidates.append(&mut rest);
    *files = candidates;
}

/// V71-D1 — rank the lexical lane's files by matched-atom RARITY, in place.
///
/// The order this replaces is `store::list_files`' `ORDER BY path` —
/// alphabetical, i.e. not a relevance signal at all, and (with the 300 ms
/// budget) one whose tail is silently unsearched. The replacement is derived
/// entirely from the hits already in hand, with no index, no store read and
/// nothing learned or personal:
///
/// 1. every match contributes the atoms of the text it ACTUALLY matched
///    (`matcher::identifier_atoms` — the whole identifier AND its camel/
///    snake sub-tokens, the dual tokenisation the 2026 BM25-over-code result
///    calls for). Reading the matched text rather than re-tokenising the
///    QUERY is what makes this work identically for a literal and for a
///    regex, and what keeps regex metacharacters out of the atom set;
/// 2. an atom's document frequency is counted across the RESULT SET, and
///    weighted by [`matcher::rarity_weight`] — the flat-IDF-tail correction:
///    a file matching a rare identifier outranks one matching a ubiquitous
///    one;
/// 3. a file's score sums its atoms' weights, each scaled by a damped
///    occurrence count, so "more hits on the same rare atom" ranks above
///    "one hit", without a 50-match file swamping everything (the per-file
///    cap already bounds it, and `ln` damps what is left).
///
/// Ties break on path, so the order stays deterministic — a query whose
/// atoms are all equally common degrades exactly to the alphabetical order
/// it replaced.
pub(crate) fn rank_by_rarity(results: &mut [TextFileResult]) {
    if results.len() < 2 {
        return;
    }
    // Per-file atom → occurrence count.
    let per_file: Vec<std::collections::HashMap<String, usize>> = results
        .iter()
        .map(|file| {
            let mut counts: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            for m in &file.matches {
                let (start, end) = m.byte_range;
                let Some(text) = m.line.get(start..end) else {
                    continue;
                };
                for atom in super::matcher::identifier_atoms(text) {
                    *counts.entry(atom).or_insert(0) += 1;
                }
            }
            counts
        })
        .collect();

    let n = results.len();
    let mut df: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for counts in &per_file {
        for atom in counts.keys() {
            *df.entry(atom.as_str()).or_insert(0) += 1;
        }
    }

    let mut scored: Vec<(f64, usize)> = per_file
        .iter()
        .enumerate()
        .map(|(i, counts)| {
            let score: f64 = counts
                .iter()
                .map(|(atom, occurrences)| {
                    let weight = super::matcher::rarity_weight(
                        df.get(atom.as_str()).copied().unwrap_or(0),
                        n,
                    );
                    weight * (1.0 + (1.0 + *occurrences as f64).ln())
                })
                .sum();
            (score, i)
        })
        .collect();
    // Stable, total order: score desc, then the pre-existing (path) order,
    // which `list_files` already guarantees is sorted.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    let order: Vec<usize> = scored.into_iter().map(|(_, i)| i).collect();
    apply_permutation(results, &order);
}

/// Reorder `items` so that `items[i]` becomes `items[order[i]]`'s old value.
/// A plain index-cycle walk — no clone of the (potentially large) match
/// vectors.
fn apply_permutation<T>(items: &mut [T], order: &[usize]) {
    debug_assert_eq!(items.len(), order.len());
    let mut position: Vec<usize> = vec![0; order.len()];
    for (new_idx, &old_idx) in order.iter().enumerate() {
        position[old_idx] = new_idx;
    }
    // `position[old] = new` — walk cycles, swapping into place.
    for i in 0..items.len() {
        while position[i] != i {
            let target = position[i];
            items.swap(i, target);
            position.swap(i, target);
        }
    }
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
    use crate::search::LaneOpts;
    use std::fs;

    /// A `LaneOpts` carrying just the `path:`/`repo:` PRE-filter.
    fn opts_path(filter: &str) -> LaneOpts<'_> {
        LaneOpts {
            path_filter: Some(filter),
            ..Default::default()
        }
    }

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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &LaneOpts::default(),
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
            &opts_path("keep/"),
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
            &opts_path("src/"),
        )
        .unwrap();
        assert_eq!(resp.results.len(), 1, "got {resp:?}");
        assert_eq!(resp.results[0].path, "Src/widget.rs");
    }

    // --- V71-D1b: candidate-first scan order + honest scanned/total --------

    fn file_row(path: &str) -> crate::store::FileRow {
        crate::store::FileRow {
            path: path.to_string(),
            blob_hash: "h".to_string(),
            lang: "rust".to_string(),
            size: 0,
        }
    }

    #[test]
    fn reorder_candidates_first_uses_the_query_tokens_with_no_hint() {
        let mut files = vec![file_row("aaa.rs"), file_row("widget_service.rb")];
        reorder_candidates_first(&mut files, "widget", None);
        assert_eq!(files[0].path, "widget_service.rb", "got {files:?}");
        assert_eq!(files[1].path, "aaa.rs");
    }

    #[test]
    fn reorder_candidates_first_uses_a_hinted_path_the_query_text_never_names() {
        // The real defect's exact shape: the bench query `/def
        // total_quantity/` expects `app/models/fiscal_entry.rb`, whose PATH
        // contains neither "total" nor "quantity" — only a SYMBOL defined
        // there does. `reorder_candidates_first` never sees a symbol table;
        // it trusts whatever `candidate_paths` the caller computed from one.
        let mut files = vec![
            file_row("aaa.rs"),
            file_row("app/models/fiscal_entry.rb"),
            file_row("bbb.rs"),
        ];
        let mut hinted = std::collections::HashSet::new();
        hinted.insert("app/models/fiscal_entry.rb".to_string());
        reorder_candidates_first(&mut files, "def total_quantity", Some(&hinted));
        assert_eq!(files[0].path, "app/models/fiscal_entry.rb", "got {files:?}");
    }

    #[test]
    fn reorder_candidates_first_is_a_stable_no_op_with_no_signal_at_all() {
        let mut files = vec![file_row("aaa.rs"), file_row("bbb.rs"), file_row("ccc.rs")];
        let before = files.clone();
        reorder_candidates_first(&mut files, "xyz_unrelated_token", None);
        assert_eq!(
            files, before,
            "no query/hint overlap must not reorder anything"
        );
    }

    #[test]
    fn reorder_candidates_first_keeps_each_groups_relative_order() {
        // Two candidates and two non-candidates, interleaved — both groups
        // must keep THEIR OWN relative order (a stable partition), not just
        // "some candidate first".
        let mut files = vec![
            file_row("z_decoy.rs"),
            file_row("a_widget.rs"),
            file_row("m_decoy.rs"),
            file_row("b_widget.rs"),
        ];
        reorder_candidates_first(&mut files, "widget", None);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["a_widget.rs", "b_widget.rs", "z_decoy.rs", "m_decoy.rs"],
            "got {paths:?}"
        );
    }

    #[test]
    fn scanned_and_total_count_the_eligible_set_when_nothing_trips() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        seed(&store, root.path(), repo_id, "a.rs", "rust", b"needle\n");
        seed(&store, root.path(), repo_id, "b.rs", "rust", b"nothing\n");
        // A skipped tier must not inflate `total` — it was never eligible.
        seed(
            &store,
            root.path(),
            repo_id,
            "big.bin",
            ingest::TIER_TOO_LARGE,
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
            &LaneOpts::default(),
        )
        .unwrap();
        assert_eq!(resp.total, 2, "big.bin's tier must be excluded: {resp:?}");
        assert_eq!(resp.scanned, 2, "both eligible files were opened: {resp:?}");
        assert_eq!(resp.results.len(), 1);
    }

    #[test]
    fn a_tripped_budget_reports_total_without_having_scanned_it() {
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
            &LaneOpts::default(),
        )
        .unwrap();
        assert!(resp.time_budget_exceeded);
        assert_eq!(
            resp.scanned, 0,
            "the deadline tripped before opening anything"
        );
        assert_eq!(
            resp.total, 1,
            "the file was still ELIGIBLE — a zero-row response must never look \
             like there was nothing to search"
        );
    }

    #[test]
    fn candidate_hint_lets_a_symbol_named_hit_survive_a_cap_that_would_otherwise_drop_it() {
        // Deterministic stand-in for the real bench defect (a tight WALL-
        // CLOCK budget missing a late-alphabetical file) using the hard
        // MAX_TOTAL_MATCHES cap instead of timing, so this test cannot be
        // flaky on a slow disk: enough alphabetically-earlier decoy files to
        // exactly exhaust the total cap (each capped individually at
        // MAX_MATCHES_PER_FILE, so no single file can absorb it alone) scan
        // first, and only the candidate-first reorder lets the real target
        // be scanned before the cap trips.
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let repo_id = store
            .upsert_repo("r", root.path().to_str().unwrap())
            .unwrap();
        let mut per_decoy = String::new();
        for _ in 0..MAX_MATCHES_PER_FILE {
            per_decoy.push_str("needle\n");
        }
        let n_decoys = MAX_TOTAL_MATCHES / MAX_MATCHES_PER_FILE;
        for i in 0..n_decoys {
            seed(
                &store,
                root.path(),
                repo_id,
                &format!("aaa_decoy_{i:02}.rs"),
                "rust",
                per_decoy.as_bytes(),
            );
        }
        seed(
            &store,
            root.path(),
            repo_id,
            "zzz_target.rs",
            "rust",
            b"needle\n",
        );

        let without_hint = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            &LaneOpts::default(),
        )
        .unwrap();
        assert!(
            without_hint.truncated,
            "sanity check failed — the decoys must exhaust MAX_TOTAL_MATCHES: {without_hint:?}"
        );
        assert!(
            !without_hint
                .results
                .iter()
                .any(|r| r.path == "zzz_target.rs"),
            "sanity check failed — the decoys must exhaust the cap first: {without_hint:?}"
        );

        let mut hinted = std::collections::HashSet::new();
        hinted.insert("zzz_target.rs".to_string());
        let opts = LaneOpts {
            candidate_paths: Some(&hinted),
            ..Default::default()
        };
        let with_hint = search_text(
            &store,
            root.path(),
            repo_id,
            "needle",
            false,
            true,
            DEFAULT_TIME_BUDGET,
            &opts,
        )
        .unwrap();
        assert!(
            with_hint.results.iter().any(|r| r.path == "zzz_target.rs"),
            "the hinted file must be scanned BEFORE the cap trips: {with_hint:?}"
        );
    }
}
