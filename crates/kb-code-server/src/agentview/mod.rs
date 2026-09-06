//! W5.1 + W5.2 — the AGENT CONTEXT VERBS: a small family of read-only,
//! token-budget-aware surfaces purpose-built for an agent (not a human
//! reader) driving kb-code — `map`/`pack`/`defs`/`refs`/`similar`/`impact`.
//! Every JSON shape here is versioned-and-golden, exactly like `join::
//! ladder`'s `join/1` (`schema` field, bump only alongside a deliberate,
//! documented shape change).
//!
//! - [`map`] — a ranked, token-budgeted outline of a directory/repo
//!   (`GET /api/map`, `kb-code map`): aider-repomap-style, but v1-rank
//!   (frecency + symbol count + path-depth penalty), NOT graph-rank — see
//!   that module's doc for exactly what's deferred and why.
//! - [`pack`] — a context PACK for a caller-given set of files: each file's
//!   map outline + provenance summary + (forward-compatible, currently
//!   empty) annotations + recent story entries, THEN as much file content
//!   as the budget allows (`GET /api/pack`, `kb-code pack`) — summaries and
//!   pointers over an unbounded dump.
//! - [`xref`] — `defs`/`refs`: a symbols-table exact/fuzzy lookup and a
//!   plain word-boundary text-grep respectively (`GET /api/defs`,
//!   `GET /api/xrefs`) — explicitly TAGS-TIER, never a real reference-graph
//!   resolution; see that module's doc for the honesty labeling (the HTTP
//!   path is `/api/xrefs`, not `/api/refs` — that path was independently
//!   claimed by W4.1's git ref-picker; see `router.rs`'s module doc).
//! - [`similar`] — nearest semantic-lane chunks to a given span
//!   (`GET /api/similar`), excluding the span's own source location; 400s
//!   with a hint when the semantic lane isn't enabled, same convention as
//!   `GET /api/search/semantic`.
//! - [`impact`] — a co-change neighborhood for one file, PLUS a cheap
//!   textual "who mentions me" signal (`GET /api/impact`) — both halves
//!   approximate, neither a real dependency-graph resolution.
//!
//! All six routes sit on the ordinary `auth_bearer`-gated `/api` nest
//! (`router.rs`), like every other browsing/provenance route in this crate
//! — nothing here is more sensitive than what `GET /api/file`/`GET
//! /api/why` already expose.

pub mod impact;
pub mod map;
pub mod pack;
pub mod similar;
pub mod xref;

use crate::config::RepoEntry;
use crate::routes::ApiError;
use axum::http::StatusCode;

/// `chars / 4` — the SAME documented, dependency-free token approximation
/// `semantic::chunk::CHARS_PER_TOKEN` uses (no real tokenizer dependency;
/// good enough to decide a greedy budget-fill cutoff, never used for
/// anything embedding-correctness sensitive). Kept as its own small copy
/// here (not imported from `semantic::chunk`, which is private to that
/// module) rather than promoting that module's constant to `pub(crate)`
/// for a single arithmetic identity.
pub(crate) fn approx_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

/// Read `path`'s CURRENT working-tree bytes off disk — never the git ODB;
/// `map`/`pack`/`similar` all want to see whatever is on disk right now,
/// exactly like `routes::file`'s no-`ref` branch. Duplicated in miniature
/// here rather than importing `routes::read_repo_file` (private to that
/// module, and bundles a ref-vs-working-tree branch none of this module's
/// callers need — they only ever want the working tree).
pub(crate) fn read_working_tree_file(repo: &RepoEntry, path: &str) -> Result<Vec<u8>, ApiError> {
    // V70-A2 — `pack`/`map`/`similar` are CONTENT-RETURNING routes, so
    // they get the same ladder `routes::read_repo_file` does: the secret
    // denylist (a typed 403 naming the matched pattern) and canonicalising
    // path containment. This module's "duplicated in miniature" note above
    // is exactly why the two guards are named here rather than assumed.
    crate::security::secrets::builtin_policy().check(path)?;
    let abs = crate::security::paths::contained_abs_path(&repo.path, path)?;
    std::fs::read(&abs).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found(format!("{path}: not found in the working tree"))
        } else {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read {path}: {e}"),
            )
        }
    })
}

/// `\b<escaped>\b` — a word-boundary regex pattern for exact-identifier
/// text search (`xref::refs`'s symbol lookup and `impact`'s "mentions"
/// signal), both riding `search::text::search_text`'s `regex = true` mode
/// (the SAME grep-searcher/grep-regex engine ripgrep itself uses, which
/// supports the `\b` assertion). Escapes every regex metacharacter in `s`
/// first so a symbol name containing one (rare, but e.g. Ruby's `foo?`/
/// `bar!`, or a C++ `operator[]`) can't corrupt the pattern or explode into
/// an unintended regex construct.
pub(crate) fn word_boundary_pattern(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    format!(r"\b{escaped}\b")
}

/// `path`'s directory depth — the number of `/`-separated segments before
/// the final one (a root-level file is depth 0). `map`'s v1-rank depth
/// penalty; forward-slash only — every `files.path`/`Symbol` path in this
/// crate is already forward-slash-normalized end to end (see `store.rs`'s
/// module doc), never a raw OS separator.
pub(crate) fn path_depth(path: &str) -> usize {
    path.matches('/').count()
}

/// A GENEROUS wall-clock budget for `search::text::search_text` calls made
/// by `xref::refs` and `impact`'s "mentions" pass — deliberately NOT
/// `search::text::DEFAULT_TIME_BUDGET` (300ms, tuned for the interactive
/// search-as-you-type lane's own p50 latency target). Both are AGENT
/// context verbs: a caller wants a complete answer far more than
/// sub-second UI responsiveness, so this trades latency for completeness.
/// Each caller's own response still carries an honest `time_budget_exceeded`
/// (`xref::RefsOut`) signal (or the blanket `approximate: true`, for
/// `impact`) if even this is exceeded on a genuinely enormous repo.
pub(crate) const TEXT_SEARCH_TIME_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approx_tokens_rounds_up() {
        assert_eq!(approx_tokens(""), 0);
        assert_eq!(approx_tokens("ab"), 1);
        assert_eq!(approx_tokens("abcd"), 1);
        assert_eq!(approx_tokens("abcde"), 2);
        assert_eq!(approx_tokens(&"x".repeat(400)), 100);
    }

    #[test]
    fn word_boundary_pattern_escapes_metacharacters() {
        assert_eq!(word_boundary_pattern("foo"), r"\bfoo\b");
        assert_eq!(word_boundary_pattern("foo?"), r"\bfoo\?\b");
        assert_eq!(word_boundary_pattern("a.b"), r"\ba\.b\b");
        assert_eq!(word_boundary_pattern("a[b]"), r"\ba\[b\]\b");
    }

    #[test]
    fn path_depth_counts_separators_before_the_filename() {
        assert_eq!(path_depth("lib.rs"), 0);
        assert_eq!(path_depth("src/lib.rs"), 1);
        assert_eq!(path_depth("src/agentview/map.rs"), 2);
    }
}
