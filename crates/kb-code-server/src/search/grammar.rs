//! W2.4 — the Search-Everywhere box's query grammar: a pure (no I/O, no
//! `axum`), deterministic parser from a raw `q=` string to a [`ParsedQuery`]
//! — which lane(s) to run, the lane-facing query text (prefix + filter
//! tokens stripped), and the parsed [`Filters`]. [`unified`](super::unified)
//! is the only caller; keeping this module pure means the grammar itself is
//! golden-pinned by plain `assert_eq!` on [`ParsedQuery`], with no daemon
//! boot required.
//!
//! # Lane prefixes
//!
//! A query may open with exactly one lane-selecting prefix — checked in this
//! order (longest/most-specific first, since `"~~"` is itself prefixed by
//! `"~"`):
//!
//! | prefix | lane | example |
//! |---|---|---|
//! | `~~` | [`Lane::Transcripts`] | `~~gorgonzola bug` |
//! | `~` | [`Lane::Sessions`] | `~fixed the gizmo race` |
//! | `@` | [`Lane::Symbols`] | `@GitRepo::open` |
//! | `#` | [`Lane::Files`] | `#config.rs` |
//! | `/` | [`Lane::Text`] | `/fn\s+add/i` (see below) |
//! | `?nl` (then a space or end-of-string) | [`Lane::Semantic`] | `?nl how does auth work` |
//! | *(none of the above)* | every lane | `add widget` |
//!
//! `?nl` is checked as a WHOLE-WORD prefix — `"?nlfoo"` (no space, no
//! end-of-string right after `nl`) does NOT match; the leading `?` stays
//! part of the literal query text and the request routes to every lane
//! instead. This is a deliberate boundary, not an oversight — pinned by
//! [`tests::bare_question_mark_without_nl_is_not_a_semantic_prefix`].
//!
//! # The `/pattern[/flags]` text prefix
//!
//! Two forms:
//!
//! - **Delimited** — a second (unescaped) `/` closes the pattern; anything
//!   between the two slashes is the pattern VERBATIM (spaces and all, so a
//!   pattern like `/fn add/` round-trips as `"fn add"`, not two filter
//!   tokens). A run of ASCII letters immediately after the closing `/` is
//!   read as flags — only `i` is recognised (case-insensitive, i.e.
//!   [`Filters::case`] `= Some(false)`); any other flag letter is accepted
//!   but ignored (forwards-compatible, not a parse error). This form is
//!   ALWAYS [`TextMode::Regex`], even if the pattern fails to compile — the
//!   delimiter is an explicit "this is a regex" signal from the caller, and
//!   a bad pattern surfaces as that lane's own `unavailable_reason` at
//!   execution time, not a grammar-level failure.
//! - **Bare** — no closing `/`. The remainder is whitespace-tokenised and
//!   filter-extracted exactly like every other lane's query text (so
//!   `/needle lang:rust` strips the `lang:` filter same as any other
//!   prefix), then the REJOINED remaining tokens are tried as a regex: if
//!   they compile, [`TextMode::Regex`]; if not (e.g. an unbalanced group or
//!   a leading bare quantifier), [`TextMode::Literal`] — "bare `/lit` is
//!   literal if the regex parse fails," per the design brief.
//!
//! # Trailing filters
//!
//! `lang:<id>`, `path:<substr>`, `repo:<name>`, `case:<yes|no>` — recognised
//! as whitespace-separated tokens ANYWHERE in the (post-prefix) query text,
//! not only at the end despite the name (a client typing `lang:rust widget`
//! should work the same as `widget lang:rust`). A repeated key overwrites
//! (last one wins). A token that LOOKS like `key:` but has an empty value
//! (`"lang:"`) or an unrecognised `case:` value (anything other than
//! `yes`/`true`/`1`/`no`/`false`/`0`) is left in place as an ordinary query
//! word rather than silently dropped — this matters for `case:`, whose
//! grammar overlaps with plausible search terms (`case:closed` is a
//! reasonable thing to search FOR). Which lanes actually consult which
//! filter is [`unified`](super::unified)'s job (files/symbols/text: `lang`+
//! `path`; text only: `case`; every repo-scoped lane: `repo`, overriding the
//! endpoint's own `?repo=`) — this module only parses, never validates lane
//! applicability.

/// One of the six search-everywhere lanes. [`LANE_ORDER`] is the box's
/// canonical, FIXED section order — every response orders its `sections` by
/// this, never by prefix-parse order or lane latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lane {
    Files,
    Symbols,
    Text,
    Semantic,
    Sessions,
    Transcripts,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::Files => "files",
            Lane::Symbols => "symbols",
            Lane::Text => "text",
            Lane::Semantic => "semantic",
            Lane::Sessions => "sessions",
            Lane::Transcripts => "transcripts",
        }
    }
}

/// The canonical section order — files, symbols, text, semantic, sessions,
/// transcripts. `unified::run` iterates this exact slice to assemble
/// `sections`, regardless of which lanes a prefix selected.
pub const LANE_ORDER: [Lane; 6] = [
    Lane::Files,
    Lane::Symbols,
    Lane::Text,
    Lane::Semantic,
    Lane::Sessions,
    Lane::Transcripts,
];

/// Parsed trailing filters — see the module doc's "Trailing filters"
/// section. Every field is `None` when the corresponding token never
/// appeared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    pub lang: Option<String>,
    pub path: Option<String>,
    pub repo: Option<String>,
    /// `Some(true)` = case-sensitive (`case:yes`), `Some(false)` =
    /// case-insensitive (`case:no`, or the `/pattern/i` flag) — only the
    /// text lane consults this.
    pub case: Option<bool>,
}

/// Whether the text lane should treat [`ParsedQuery::query`] as a regex or a
/// literal string — only meaningful when [`ParsedQuery::lanes`] contains
/// [`Lane::Text`]; every other lane ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMode {
    Literal,
    Regex,
}

/// The result of parsing one raw `q=` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    /// The lanes to run, ALREADY in [`LANE_ORDER`]'s canonical order — a
    /// subset when a prefix selected one lane, or the full six-lane set
    /// when no prefix was given. Never empty (a fully-empty raw `q` is
    /// handled by `unified::run` itself, before this parser ever runs — see
    /// that module's doc — so `parse` is never called with `""`, though it
    /// tolerates it: an empty raw string with no prefix still yields the
    /// full lane set with an empty query, matching every other prefix's
    /// empty-remainder behaviour).
    pub lanes: Vec<Lane>,
    /// The lane-facing query text: prefix and filter tokens stripped,
    /// single-space-joined, trimmed. May be empty (e.g. a bare `@` or `~~`
    /// with nothing after it) — each lane runner decides what an empty
    /// query means for IT (files falls back to "recent"; every other lane
    /// reports `unavailable_reason`).
    pub query: String,
    pub filters: Filters,
    pub text_mode: TextMode,
}

/// Parse `raw` per the module doc. Total function — every input, including
/// `""` or pure whitespace, produces a `ParsedQuery` (never panics, never
/// errors); "is this actually searchable" is a downstream, per-lane
/// decision, not this parser's job.
pub fn parse(raw: &str) -> ParsedQuery {
    let raw = raw.trim();

    if let Some(rest) = raw.strip_prefix("~~") {
        let (query, filters) = extract_filters(rest.trim_start());
        return ParsedQuery {
            lanes: vec![Lane::Transcripts],
            query,
            filters,
            text_mode: TextMode::Literal,
        };
    }
    if let Some(rest) = raw.strip_prefix('~') {
        let (query, filters) = extract_filters(rest.trim_start());
        return ParsedQuery {
            lanes: vec![Lane::Sessions],
            query,
            filters,
            text_mode: TextMode::Literal,
        };
    }
    if let Some(rest) = raw.strip_prefix('@') {
        let (query, filters) = extract_filters(rest.trim_start());
        return ParsedQuery {
            lanes: vec![Lane::Symbols],
            query,
            filters,
            text_mode: TextMode::Literal,
        };
    }
    if let Some(rest) = raw.strip_prefix('#') {
        let (query, filters) = extract_filters(rest.trim_start());
        return ParsedQuery {
            lanes: vec![Lane::Files],
            query,
            filters,
            text_mode: TextMode::Literal,
        };
    }
    if let Some(rest) = raw.strip_prefix('/') {
        let (query, filters, text_mode) = parse_text_prefix(rest);
        return ParsedQuery {
            lanes: vec![Lane::Text],
            query,
            filters,
            text_mode,
        };
    }
    if let Some(rest) = raw.strip_prefix("?nl") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            let (query, filters) = extract_filters(rest.trim_start());
            return ParsedQuery {
                lanes: vec![Lane::Semantic],
                query,
                filters,
                text_mode: TextMode::Literal,
            };
        }
        // "?nlfoo" — not a valid `?nl` prefix (no word boundary); falls
        // through to the no-prefix / all-lanes branch below, verbatim.
    }

    let (query, filters) = extract_filters(raw);
    ParsedQuery {
        lanes: LANE_ORDER.to_vec(),
        query,
        filters,
        text_mode: TextMode::Literal,
    }
}

/// The recognised filter-token keys — anything else stays a literal query
/// word.
const FILTER_KEYS: [&str; 4] = ["lang", "path", "repo", "case"];

/// Whitespace-tokenise `s`, pull out any `key:value` token whose `key` is
/// one of [`FILTER_KEYS`] and whose `value` is non-empty (and, for `case:`,
/// recognised — see the module doc), and rejoin the remaining tokens with a
/// single space. A repeated key overwrites (last occurrence wins).
fn extract_filters(s: &str) -> (String, Filters) {
    let mut filters = Filters::default();
    let mut kept: Vec<&str> = Vec::new();

    for tok in s.split_whitespace() {
        let mut consumed = false;
        for key in FILTER_KEYS {
            let Some(value) = tok.strip_prefix(key).and_then(|r| r.strip_prefix(':')) else {
                continue;
            };
            if value.is_empty() {
                break; // `"lang:"` etc. — not a filter, fall through to `kept`.
            }
            match key {
                "lang" => {
                    filters.lang = Some(value.to_string());
                    consumed = true;
                }
                "path" => {
                    filters.path = Some(value.to_string());
                    consumed = true;
                }
                "repo" => {
                    filters.repo = Some(value.to_string());
                    consumed = true;
                }
                "case" => match value {
                    "yes" | "true" | "1" => {
                        filters.case = Some(true);
                        consumed = true;
                    }
                    "no" | "false" | "0" => {
                        filters.case = Some(false);
                        consumed = true;
                    }
                    _ => {} // unrecognised value — not a filter, kept as a word.
                },
                _ => unreachable!("FILTER_KEYS is exhaustively matched above"),
            }
            break;
        }
        if !consumed {
            kept.push(tok);
        }
    }

    (kept.join(" "), filters)
}

/// Parse the remainder after a leading `/` — see the module doc's
/// "`/pattern[/flags]`" section.
fn parse_text_prefix(rest: &str) -> (String, Filters, TextMode) {
    if let Some(close) = rest.find('/') {
        let pattern = &rest[..close];
        let after = &rest[close + 1..];
        let flags_end = after
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        let flags = &after[..flags_end];
        // Whatever follows the flags run (further trailing filter tokens,
        // if any) is filter-extracted; the resulting `kept` text is
        // discarded — the delimited pattern is already fully specified and
        // never gets extra words appended to it.
        let (_, mut filters) = extract_filters(after[flags_end..].trim_start());
        if flags.contains('i') && filters.case.is_none() {
            filters.case = Some(false);
        }
        (pattern.to_string(), filters, TextMode::Regex)
    } else {
        let (query, filters) = extract_filters(rest);
        let mode = if grep_regex::RegexMatcherBuilder::new().build(&query).is_ok() {
            TextMode::Regex
        } else {
            TextMode::Literal
        };
        (query, filters, mode)
    }
}

/// Every filter key this grammar recognises — exposed for this module's own
/// tests to cross-check against [`FILTER_KEYS`] without making the const
/// itself `pub`.
#[cfg(test)]
pub(crate) fn filter_keys() -> std::collections::HashSet<&'static str> {
    FILTER_KEYS.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> Filters {
        Filters::default()
    }

    #[test]
    fn no_prefix_routes_to_every_lane_in_canonical_order() {
        let p = parse("add widget");
        assert_eq!(p.lanes, LANE_ORDER.to_vec());
        assert_eq!(p.query, "add widget");
        assert_eq!(p.filters, f());
        assert_eq!(p.text_mode, TextMode::Literal);
    }

    #[test]
    fn symbols_prefix() {
        let p = parse("@GitRepo::open");
        assert_eq!(p.lanes, vec![Lane::Symbols]);
        assert_eq!(p.query, "GitRepo::open");
        assert_eq!(p.filters, f());
    }

    #[test]
    fn files_prefix() {
        let p = parse("#config.rs");
        assert_eq!(p.lanes, vec![Lane::Files]);
        assert_eq!(p.query, "config.rs");
    }

    #[test]
    fn sessions_prefix() {
        let p = parse("~fixed the gizmo race");
        assert_eq!(p.lanes, vec![Lane::Sessions]);
        assert_eq!(p.query, "fixed the gizmo race");
    }

    #[test]
    fn transcripts_prefix_is_checked_before_the_single_tilde() {
        let p = parse("~~gorgonzola bug");
        assert_eq!(p.lanes, vec![Lane::Transcripts]);
        assert_eq!(p.query, "gorgonzola bug");
    }

    #[test]
    fn semantic_prefix_with_space() {
        let p = parse("?nl how does auth work");
        assert_eq!(p.lanes, vec![Lane::Semantic]);
        assert_eq!(p.query, "how does auth work");
    }

    #[test]
    fn semantic_prefix_alone() {
        let p = parse("?nl");
        assert_eq!(p.lanes, vec![Lane::Semantic]);
        assert_eq!(p.query, "");
    }

    #[test]
    fn bare_question_mark_without_nl_is_not_a_semantic_prefix() {
        let p = parse("?nlnotarealprefix");
        assert_eq!(p.lanes, LANE_ORDER.to_vec());
        assert_eq!(p.query, "?nlnotarealprefix");

        let p2 = parse("?something else");
        assert_eq!(p2.lanes, LANE_ORDER.to_vec());
        assert_eq!(p2.query, "?something else");
    }

    #[test]
    fn text_prefix_delimited_pattern_with_embedded_space_is_regex() {
        let p = parse("/fn add/");
        assert_eq!(p.lanes, vec![Lane::Text]);
        assert_eq!(p.query, "fn add");
        assert_eq!(p.text_mode, TextMode::Regex);
        assert_eq!(p.filters.case, None);
    }

    #[test]
    fn text_prefix_delimited_with_i_flag_sets_case_insensitive() {
        let p = parse("/fn\\s+add/i");
        assert_eq!(p.lanes, vec![Lane::Text]);
        assert_eq!(p.query, "fn\\s+add");
        assert_eq!(p.text_mode, TextMode::Regex);
        assert_eq!(p.filters.case, Some(false));
    }

    #[test]
    fn text_prefix_delimited_with_trailing_filters_after_flags() {
        let p = parse("/fn add/ lang:rust");
        assert_eq!(p.lanes, vec![Lane::Text]);
        assert_eq!(p.query, "fn add");
        assert_eq!(p.filters.lang.as_deref(), Some("rust"));
        assert_eq!(p.text_mode, TextMode::Regex);
    }

    #[test]
    fn text_prefix_delimited_is_always_regex_even_if_invalid() {
        // "*abc" (leading bare quantifier) does not compile — the DELIMITED
        // form is still Regex (an explicit signal), never silently
        // downgraded to Literal; a bad pattern is that lane's problem at
        // execution time, not the grammar's.
        let p = parse("/*abc/");
        assert_eq!(p.text_mode, TextMode::Regex);
        assert_eq!(p.query, "*abc");
    }

    #[test]
    fn text_prefix_bare_valid_regex_stays_regex() {
        let p = parse("/needle");
        assert_eq!(p.lanes, vec![Lane::Text]);
        assert_eq!(p.query, "needle");
        assert_eq!(p.text_mode, TextMode::Regex);
    }

    #[test]
    fn text_prefix_bare_invalid_regex_falls_back_to_literal() {
        // Leading `*` is not a valid regex (repetition operator with
        // nothing to repeat) — the bare form's documented fallback.
        let p = parse("/*abc");
        assert_eq!(p.lanes, vec![Lane::Text]);
        assert_eq!(p.query, "*abc");
        assert_eq!(p.text_mode, TextMode::Literal);
    }

    #[test]
    fn text_prefix_bare_strips_filters_before_the_regex_check() {
        let p = parse("/needle lang:rust");
        assert_eq!(p.query, "needle");
        assert_eq!(p.filters.lang.as_deref(), Some("rust"));
        assert_eq!(p.text_mode, TextMode::Regex);
    }

    #[test]
    fn lang_path_repo_filters_are_stripped_from_the_all_lanes_query() {
        let p = parse("widget lang:rust path:src/ repo:kb");
        assert_eq!(p.lanes, LANE_ORDER.to_vec());
        assert_eq!(p.query, "widget");
        assert_eq!(p.filters.lang.as_deref(), Some("rust"));
        assert_eq!(p.filters.path.as_deref(), Some("src/"));
        assert_eq!(p.filters.repo.as_deref(), Some("kb"));
    }

    #[test]
    fn case_filter_yes_and_no() {
        assert_eq!(parse("widget case:yes").filters.case, Some(true));
        assert_eq!(parse("widget case:no").filters.case, Some(false));
        assert_eq!(parse("widget case:true").filters.case, Some(true));
        assert_eq!(parse("widget case:0").filters.case, Some(false));
    }

    #[test]
    fn case_filter_unrecognised_value_stays_a_literal_word() {
        let p = parse("case:closed widget");
        assert_eq!(p.filters.case, None);
        assert_eq!(p.query, "case:closed widget");
    }

    #[test]
    fn empty_filter_value_stays_a_literal_word() {
        let p = parse("widget lang:");
        assert_eq!(p.filters.lang, None);
        assert_eq!(p.query, "widget lang:");
    }

    #[test]
    fn repeated_filter_key_last_one_wins() {
        let p = parse("widget lang:rust lang:python");
        assert_eq!(p.filters.lang.as_deref(), Some("python"));
    }

    #[test]
    fn prefix_and_filters_combine() {
        let p = parse("@Widget lang:rust");
        assert_eq!(p.lanes, vec![Lane::Symbols]);
        assert_eq!(p.query, "Widget");
        assert_eq!(p.filters.lang.as_deref(), Some("rust"));
    }

    #[test]
    fn bare_prefix_with_nothing_after_yields_an_empty_query() {
        for (raw, lane) in [
            ("@", Lane::Symbols),
            ("#", Lane::Files),
            ("~", Lane::Sessions),
            ("~~", Lane::Transcripts),
        ] {
            let p = parse(raw);
            assert_eq!(p.lanes, vec![lane], "prefix {raw:?}");
            assert_eq!(p.query, "", "prefix {raw:?}");
        }
    }

    #[test]
    fn leading_and_trailing_whitespace_is_trimmed_before_prefix_detection() {
        let p = parse("   @Widget   ");
        assert_eq!(p.lanes, vec![Lane::Symbols]);
        assert_eq!(p.query, "Widget");
    }

    #[test]
    fn filter_keys_matches_the_documented_set() {
        let expected: std::collections::HashSet<&'static str> =
            ["lang", "path", "repo", "case"].into_iter().collect();
        assert_eq!(filter_keys(), expected);
    }
}
