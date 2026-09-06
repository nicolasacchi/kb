//! **kbcq/1** — the ONE query grammar (W2.4's parser, grown by V71-D1 per
//! design D3: "the existing `search/grammar.rs` grows, never gets
//! replaced"). A pure (no I/O, no `axum`), TOTAL parser from a raw `q=`
//! string to a [`ParsedQuery`] — which lane(s) to run, the lane-facing
//! query text (prefix + filter tokens stripped), the parsed [`Filters`], the
//! query MODIFIERS (`sort:`/`explain:`/`group:`/`facets:`), a non-fatal
//! [`Diagnostic`] list,
//! and the [`ParsedQuery::normalized`] re-rendering of what actually ran.
//! [`unified`](super::unified) is the only in-crate caller; keeping this
//! module pure means the grammar is golden-pinned by plain `assert_eq!`,
//! with no daemon boot required.
//!
//! # One grammar, two implementations, one golden
//!
//! The SPA parses the same string client-side (chips, the query bar's live
//! typeahead, the CLI-line affordance), so kbcq/1 has a TS mirror at
//! `web-code/src/lib/kbcq.ts`. Neither is derived from the other — they are
//! pinned in LOCK-STEP by ONE shared fixture, `crates/kb-code-server/
//! grammar/kbcq.golden.json`, which
//! [`tests::golden_corpus_matches_the_rust_parser`] and
//! `web-code/src/lib/kbcq.golden.test.ts` both walk (the same discipline
//! root CLAUDE.md #29 records for wikilinks and #35 for the gallery URL
//! grammar). Add a case to the fixture and BOTH sides must agree; add a
//! filter key and [`tests::every_declared_filter_key_has_a_consumer`] fails
//! until something actually reads it.
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
//! - **Bare** — no closing `/`. The remainder is tokenised and
//!   filter-extracted exactly like every other lane's query text (so
//!   `/needle lang:rust` strips the `lang:` filter same as any other
//!   prefix), then the REJOINED remaining tokens are tried as a regex: if
//!   they compile, [`TextMode::Regex`]; if not (e.g. an unbalanced group or
//!   a leading bare quantifier), [`TextMode::Literal`] — "bare `/lit` is
//!   literal if the regex parse fails," per the design brief.
//!
//! # Terms, quoting and filters
//!
//! Everything that is not a filter token is a TERM ([`QueryTerm`]).
//! `"like this"` is one QUOTED term: quotes protect embedded whitespace and
//! any `key:` shape inside them from filter extraction, and they are
//! stripped from [`ParsedQuery::query`] (so the lanes see the phrase, not
//! the punctuation) while [`QueryTerm::quoted`] remembers enough for
//! [`ParsedQuery::normalized`] to re-render it faithfully.
//!
//! Filters are `key:value` tokens recognised ANYWHERE in the (post-prefix)
//! query text, not only at the end. A leading `-` NEGATES a filter
//! (`-path:spec/`), for the keys whose [`FilterKeySpec::negatable`] says so.
//! A multi-valued key takes `a|b` alternation (`kind:class|module`), read as
//! "any of". A repeated single-valued key overwrites (last one wins); a
//! repeated multi-valued key accumulates.
//!
//! | key | shape | negatable | who consumes it |
//! |---|---|---|---|
//! | `lang:` | single | yes | files/symbols/text — `unified::matches_filters` |
//! | `path:` | single | yes | files/symbols/text PRE-filter — `search::path_prefilter_matches` |
//! | `repo:` | single | no | every repo-scoped lane, overriding `?repo=` |
//! | `case:` | single (`yes|true|1|no|false|0`) | no | the text lane |
//! | `ext:` | multi | yes | files/symbols/text — path extension |
//! | `kind:` | multi | yes | the symbols lane — `Symbol::kind` |
//! | `sort:` | single (`relevance|path`) | no | the returned PAGE's order |
//! | `explain:` | single (`1|0|yes|no|true|false`) | no | the box's per-lane decomposition |
//! | `group:` | single (`file|kind|lane|dir|none`) | no | how the returned page is GROUPED (V71-D2) |
//! | `facets:` | single (`1|0|yes|no|true|false`) | no | the facet census over the returned page (V71-D2) |
//!
//! A token that LOOKS like `key:` but has an empty value (`"lang:"`), an
//! unrecognised value for a closed-vocabulary key (`case:closed`), an
//! unsupported negation (`-repo:kb`) or an unknown key (`laang:rust`) is
//! LEFT IN PLACE as an ordinary query word and reported as a
//! [`Diagnostic`] — never silently dropped, never a hard failure (a
//! `case:closed` is a reasonable thing to search FOR, and a typo should
//! still return something rather than nothing). Unknown keys get a
//! did-you-mean suggestion when one is within edit distance
//! [`SUGGEST_MAX_DISTANCE`].
//!
//! Which lanes actually consult which filter is
//! [`unified`](super::unified)'s job — this module only parses, never
//! validates lane applicability.

use serde::{Deserialize, Serialize};

/// One of the six search-everywhere lanes. [`LANE_ORDER`] is the box's
/// canonical, FIXED section order — every response orders its `sections` by
/// this, never by prefix-parse order or lane latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
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

    /// The prefix that selects THIS lane alone — the inverse of the module
    /// doc's prefix table, used by [`normalize`] to re-render a parsed
    /// query. [`Lane::Text`] has no entry: its prefix carries a pattern body
    /// (`/re/flags`), so `normalize` renders that form itself.
    fn prefix(self) -> Option<&'static str> {
        match self {
            Lane::Files => Some("#"),
            Lane::Symbols => Some("@"),
            Lane::Semantic => Some("?nl "),
            Lane::Sessions => Some("~"),
            Lane::Transcripts => Some("~~"),
            Lane::Text => None,
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

/// One recognised filter key's DECLARATION — see the module doc's table.
/// [`FILTER_SPECS`] is the single home for the key set: the parser reads it,
/// [`normalize`] renders from it, the did-you-mean suggester searches it,
/// and [`tests::every_declared_filter_key_has_a_consumer`] walks it against
/// the module that must actually read each one.
#[derive(Debug, Clone, Copy)]
pub struct FilterKeySpec {
    pub key: &'static str,
    /// `true` = `a|b` alternation accumulates into a list; `false` = last
    /// occurrence wins.
    pub multi: bool,
    /// `true` = a leading `-` is honoured (and lands in the matching
    /// `not_*` field); `false` = `-key:` is a diagnostic and stays a word.
    pub negatable: bool,
    /// The closed value vocabulary, or `None` for a free-form value.
    pub values: Option<&'static [&'static str]>,
    /// The module a consumer must live in, and the Rust expression it must
    /// contain for this key to be WIRED — the dead-surface walk. A key
    /// declared here with nothing reading it fails the test suite.
    pub consumer_module: &'static str,
    pub consumer_expr: &'static str,
}

/// Every filter key kbcq/1 recognises. Adding one here without wiring a
/// consumer fails [`tests::every_declared_filter_key_has_a_consumer`] — the
/// v7.0 "silently dead surface" defect class, closed by construction for
/// this grammar.
pub const FILTER_SPECS: &[FilterKeySpec] = &[
    FilterKeySpec {
        key: "lang",
        multi: false,
        negatable: true,
        values: None,
        consumer_module: "unified.rs",
        consumer_expr: "filters.lang",
    },
    FilterKeySpec {
        key: "path",
        multi: false,
        negatable: true,
        values: None,
        consumer_module: "unified.rs",
        consumer_expr: "filters.path",
    },
    FilterKeySpec {
        key: "repo",
        multi: false,
        negatable: false,
        values: None,
        consumer_module: "unified.rs",
        consumer_expr: "filters.repo",
    },
    FilterKeySpec {
        key: "case",
        multi: false,
        negatable: false,
        values: Some(&["yes", "true", "1", "no", "false", "0"]),
        consumer_module: "unified.rs",
        consumer_expr: "filters.case",
    },
    FilterKeySpec {
        key: "ext",
        multi: true,
        negatable: true,
        values: None,
        consumer_module: "unified.rs",
        consumer_expr: "filters.ext",
    },
    FilterKeySpec {
        key: "kind",
        multi: true,
        negatable: true,
        values: None,
        consumer_module: "unified.rs",
        consumer_expr: "filters.kind",
    },
    FilterKeySpec {
        key: "sort",
        multi: false,
        negatable: false,
        values: Some(&["relevance", "path"]),
        consumer_module: "unified.rs",
        consumer_expr: "parsed.sort",
    },
    FilterKeySpec {
        key: "explain",
        multi: false,
        negatable: false,
        values: Some(&["1", "0", "yes", "no", "true", "false"]),
        consumer_module: "unified.rs",
        consumer_expr: "parsed.explain",
    },
    FilterKeySpec {
        key: "group",
        multi: false,
        negatable: false,
        values: Some(GROUP_VALUES),
        consumer_module: "unified.rs",
        consumer_expr: "parsed.group",
    },
    FilterKeySpec {
        key: "facets",
        multi: false,
        negatable: false,
        values: Some(&["1", "0", "yes", "no", "true", "false"]),
        consumer_module: "unified.rs",
        consumer_expr: "parsed.facets",
    },
];

/// Farthest a typo may sit from a known key and still earn a did-you-mean —
/// `laang:` (1) suggests `lang:`; `gorgonzola:` is left alone rather than
/// pointed at whatever happened to be nearest.
pub const SUGGEST_MAX_DISTANCE: usize = 2;

fn spec_for(key: &str) -> Option<&'static FilterKeySpec> {
    FILTER_SPECS.iter().find(|s| s.key == key)
}

/// Parsed filters — the TYPED view every lane runner reads. `lang`/`path`/
/// `repo`/`case` keep their pre-kbcq shapes and semantics byte-for-byte
/// (`None` when the token never appeared, last-occurrence-wins); the
/// kbcq/1 additions are list-shaped from birth, so alternation and
/// repetition accumulate rather than overwrite.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filters {
    pub lang: Option<String>,
    pub path: Option<String>,
    pub repo: Option<String>,
    /// `Some(true)` = case-sensitive (`case:yes`), `Some(false)` =
    /// case-insensitive (`case:no`, or the `/pattern/i` flag) — only the
    /// text lane consults this.
    pub case: Option<bool>,
    /// `ext:rb|erb` — a file-extension whitelist (no leading dot, matched
    /// case-insensitively against the path's suffix).
    pub ext: Vec<String>,
    /// `kind:class|module` — a `Symbol::kind` whitelist for the symbols
    /// lane.
    pub kind: Vec<String>,
    /// The negated forms (`-lang:rb`, `-path:spec/`, `-ext:min.js`,
    /// `-kind:const`) — an exclusion always beats an inclusion, so a
    /// candidate matching any `not_*` entry is dropped even if it also
    /// matched the positive filter.
    pub not_lang: Vec<String>,
    pub not_path: Vec<String>,
    pub not_ext: Vec<String>,
    pub not_kind: Vec<String>,
}

/// How the returned PAGE is ordered — `sort:`. Deliberately narrow: this
/// re-orders the page a lane already selected, it does NOT change which
/// candidates that lane's own ranking chose (a deterministic escape hatch
/// from the learned layer, per the design's own risk-3 mitigation — not a
/// second ranking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    Relevance,
    Path,
}

/// How the returned page is GROUPED — `group:` (V71-D2, design D3's results
/// page). Like [`SortKey`] this is a presentation of the page a lane already
/// selected: grouping never changes which candidates that lane chose, never
/// re-ranks them (rows keep their rank order inside a group, and groups come
/// out in first-appearance order) and never drops one. Every hit belongs to
/// exactly one group per key, so the groups PARTITION the page — which is
/// what lets a client render from `groups` alone without re-deriving
/// membership.
///
/// [`GroupKey::None`] is a real, nameable value rather than "omit the token":
/// a saved search or a facet-written query needs a way to SAY "ungrouped"
/// that survives a round trip through [`normalize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupKey {
    /// One group per file path — the text lane's natural shape.
    File,
    /// One group per symbol kind. Only the symbols lane carries one; every
    /// other lane's hits land in a single honestly-labelled "no kind" group
    /// rather than being silently dropped.
    Kind,
    /// One group per lane. Degenerate INSIDE a section (a section is one
    /// lane) and meaningful across the whole response.
    Lane,
    /// One group per containing directory.
    Dir,
    None,
}

impl GroupKey {
    pub fn as_str(self) -> &'static str {
        match self {
            GroupKey::File => "file",
            GroupKey::Kind => "kind",
            GroupKey::Lane => "lane",
            GroupKey::Dir => "dir",
            GroupKey::None => "none",
        }
    }
}

/// Every value `group:` accepts — the single home for the vocabulary, walked
/// by [`tests::every_declared_group_key_parses`] against the parser and by
/// `unified`'s own test against the grouper, so a value that parses but
/// groups nothing fails loudly (the v7.0 dead-surface defect class).
pub const GROUP_KEYS: [GroupKey; 5] = [
    GroupKey::File,
    GroupKey::Kind,
    GroupKey::Lane,
    GroupKey::Dir,
    GroupKey::None,
];

/// The closed value vocabulary `group:` accepts, as the strings
/// [`FILTER_SPECS`] validates against — derived from [`GROUP_KEYS`] by
/// [`tests::group_vocabulary_matches_the_group_keys`], never by hand.
const GROUP_VALUES: &[&str] = &["file", "kind", "lane", "dir", "none"];

/// Whether the text lane should treat [`ParsedQuery::query`] as a regex or a
/// literal string — only meaningful when [`ParsedQuery::lanes`] contains
/// [`Lane::Text`]; every other lane ignores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextMode {
    Literal,
    Regex,
}

/// One non-filter word of the query. `quoted` records that the author wrote
/// `"a phrase"` — the quotes are stripped from [`ParsedQuery::query`] (the
/// lanes want the text) but re-rendered by [`normalize`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryTerm {
    pub text: String,
    #[serde(default)]
    pub quoted: bool,
}

/// A non-fatal parse note. kbcq/1 NEVER fails a parse: an unknown or
/// malformed filter token is searched literally and explained here, so a
/// typo degrades to "found something, and here is what I did" rather than to
/// an empty page or a 400.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Always `"warning"` today — the field exists so a future `"info"`
    /// (e.g. "this lane refuses `ast:`") is additive on the wire.
    pub severity: String,
    /// The offending token, verbatim as the author typed it.
    pub token: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl Diagnostic {
    fn warn(token: &str, message: impl Into<String>, suggestion: Option<String>) -> Self {
        Self {
            severity: "warning".to_string(),
            token: token.to_string(),
            message: message.into(),
            suggestion,
        }
    }
}

/// The result of parsing one raw `q=` string. Deliberately NOT
/// `Deserialize`: this is a parser OUTPUT, and the only way to obtain one is
/// [`parse`] — a `ParsedQuery` deserialized from a client's JSON would be a
/// second, unvalidated door into the lane runners.
#[derive(Debug, Clone, PartialEq, Serialize)]
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
    /// quotes stripped, single-space-joined, trimmed. May be empty (e.g. a
    /// bare `@` or `~~` with nothing after it) — each lane runner decides
    /// what an empty query means for IT (files falls back to "recent"; every
    /// other lane reports `unavailable_reason`).
    pub query: String,
    /// The same text as [`Self::query`], per term, with quoting preserved.
    pub terms: Vec<QueryTerm>,
    pub filters: Filters,
    pub text_mode: TextMode,
    /// `sort:` — see [`SortKey`]. `None` = the lane's own ranking.
    pub sort: Option<SortKey>,
    /// `explain:1` — the caller asked for the ranking decomposition.
    pub explain: bool,
    /// V71-D2 — `group:` — how the returned page is grouped. `None` = the
    /// token never appeared; `Some(GroupKey::None)` = the author explicitly
    /// asked for no grouping. The two are different facts and `normalize`
    /// renders them differently, so a saved search that says "ungrouped"
    /// stays ungrouped through a round trip.
    pub group: Option<GroupKey>,
    /// V71-D2 — `facets:1` — the caller asked for the facet census over the
    /// page this response returns.
    pub facets: bool,
    /// Non-fatal parse notes, in token order — see [`Diagnostic`].
    pub diagnostics: Vec<Diagnostic>,
    /// The canonical re-rendering of everything above: what actually ran, in
    /// one string a human can paste back into the box. Parsing it yields the
    /// same query (a fixed point) — pinned by
    /// [`tests::normalize_round_trips_as_a_fixed_point`].
    pub normalized: String,
}

/// Parse `raw` per the module doc. Total function — every input, including
/// `""` or pure whitespace, produces a `ParsedQuery` (never panics, never
/// errors); "is this actually searchable" is a downstream, per-lane
/// decision, not this parser's job.
pub fn parse(raw: &str) -> ParsedQuery {
    let raw = raw.trim();

    let mut out = if let Some(rest) = raw.strip_prefix("~~") {
        finish(vec![Lane::Transcripts], extract(rest.trim_start()), None)
    } else if let Some(rest) = raw.strip_prefix('~') {
        finish(vec![Lane::Sessions], extract(rest.trim_start()), None)
    } else if let Some(rest) = raw.strip_prefix('@') {
        finish(vec![Lane::Symbols], extract(rest.trim_start()), None)
    } else if let Some(rest) = raw.strip_prefix('#') {
        finish(vec![Lane::Files], extract(rest.trim_start()), None)
    } else if let Some(rest) = raw.strip_prefix('/') {
        return parse_text_prefix(rest);
    } else if let Some(rest) = raw
        .strip_prefix("?nl")
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    {
        finish(vec![Lane::Semantic], extract(rest.trim_start()), None)
    } else {
        // "?nlfoo" — not a valid `?nl` prefix (no word boundary); falls
        // through to the no-prefix / all-lanes branch, verbatim.
        finish(LANE_ORDER.to_vec(), extract(raw), None)
    };
    out.normalized = normalize(&out);
    out
}

/// Everything [`extract`] pulls out of one post-prefix string.
struct Extracted {
    terms: Vec<QueryTerm>,
    filters: Filters,
    sort: Option<SortKey>,
    explain: bool,
    group: Option<GroupKey>,
    facets: bool,
    diagnostics: Vec<Diagnostic>,
}

fn finish(lanes: Vec<Lane>, e: Extracted, text_mode: Option<TextMode>) -> ParsedQuery {
    ParsedQuery {
        lanes,
        query: join_terms(&e.terms),
        terms: e.terms,
        filters: e.filters,
        text_mode: text_mode.unwrap_or(TextMode::Literal),
        sort: e.sort,
        explain: e.explain,
        group: e.group,
        facets: e.facets,
        diagnostics: e.diagnostics,
        // Filled in by `parse`/`parse_text_prefix` once the whole query is
        // assembled — `normalize` needs the lanes, which `finish` has only
        // just been handed.
        normalized: String::new(),
    }
}

fn join_terms(terms: &[QueryTerm]) -> String {
    terms
        .iter()
        .map(|t| t.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One raw whitespace-separated token, with its quoting still attached.
struct RawToken {
    /// The token with its quote characters REMOVED.
    text: String,
    /// The token exactly as the author typed it — used verbatim in
    /// diagnostics, so a warning names what they actually wrote.
    raw: String,
    /// A `"` appeared anywhere in this token.
    quoted: bool,
}

/// Whitespace-tokenise `s`, honouring double quotes: whitespace inside a
/// quoted run does NOT split, and the quote characters themselves are
/// dropped from [`RawToken::text`]. An unterminated quote runs to the end of
/// the string (a half-typed query is the common case in an interactive box —
/// refusing it would make the grammar fail exactly while the human is still
/// typing).
fn tokenize(s: &str) -> Vec<RawToken> {
    let mut out: Vec<RawToken> = Vec::new();
    let mut text = String::new();
    let mut raw = String::new();
    let mut quoted = false;
    let mut in_quotes = false;
    for ch in s.chars() {
        if ch == '"' {
            in_quotes = !in_quotes;
            quoted = true;
            raw.push(ch);
            continue;
        }
        if ch.is_whitespace() && !in_quotes {
            if !raw.is_empty() {
                out.push(RawToken {
                    text: std::mem::take(&mut text),
                    raw: std::mem::take(&mut raw),
                    quoted,
                });
                quoted = false;
            }
            continue;
        }
        text.push(ch);
        raw.push(ch);
    }
    if !raw.is_empty() {
        out.push(RawToken { text, raw, quoted });
    }
    out
}

/// Split one token into `(negated, key, value)` when it has a `key:value`
/// shape, else `None`. The key must be ASCII alphanumerics/`_`/`-` only, so
/// a term like `https://example.com/x` or `Foo::bar` is never mistaken for a
/// filter.
fn split_key_value(text: &str) -> Option<(bool, &str, &str)> {
    let (negated, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let colon = body.find(':')?;
    let key = &body[..colon];
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some((negated, key, &body[colon + 1..]))
}

/// The parser's core: walk the tokens, pull out the recognised filters and
/// modifiers, keep everything else as a term, and explain every token it
/// could not honour. See the module doc's "Terms, quoting and filters".
fn extract(s: &str) -> Extracted {
    let mut filters = Filters::default();
    let mut terms: Vec<QueryTerm> = Vec::new();
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut sort: Option<SortKey> = None;
    let mut explain = false;
    let mut group: Option<GroupKey> = None;
    let mut facets = false;

    fn keep(terms: &mut Vec<QueryTerm>, tok: &RawToken) {
        terms.push(QueryTerm {
            text: tok.text.clone(),
            quoted: tok.quoted,
        });
    }

    for tok in tokenize(s) {
        // A quoted token is a PHRASE, never a filter — quoting is how you
        // search for the literal text `lang:rust`.
        if tok.quoted && tok.raw.starts_with('"') {
            keep(&mut terms, &tok);
            continue;
        }
        let Some((negated, key, value)) = split_key_value(&tok.text) else {
            keep(&mut terms, &tok);
            continue;
        };
        let Some(spec) = spec_for(key) else {
            if let Some(hint) = suggest_key(key) {
                diagnostics.push(Diagnostic::warn(
                    &tok.raw,
                    format!("unknown filter `{key}:` — searched as an ordinary word"),
                    Some(format!("{hint}:{value}")),
                ));
            }
            keep(&mut terms, &tok);
            continue;
        };
        if value.is_empty() {
            diagnostics.push(Diagnostic::warn(
                &tok.raw,
                format!("`{key}:` has no value — searched as an ordinary word"),
                None,
            ));
            keep(&mut terms, &tok);
            continue;
        }
        if negated && !spec.negatable {
            diagnostics.push(Diagnostic::warn(
                &tok.raw,
                format!("`{key}:` cannot be negated — searched as an ordinary word"),
                None,
            ));
            keep(&mut terms, &tok);
            continue;
        }
        let values: Vec<&str> = if spec.multi {
            value.split('|').filter(|v| !v.is_empty()).collect()
        } else {
            vec![value]
        };
        // A closed vocabulary is matched case-INSENSITIVELY, and the value
        // is then canonicalised to the vocabulary's own spelling before it
        // reaches `apply_filter` — otherwise `case:YES` would pass
        // validation and then fail the exact match inside `apply_filter`,
        // silently meaning `case:no`.
        let mut values = values;
        let canonical: Vec<&'static str>;
        if let Some(vocab) = spec.values {
            let mut canon: Vec<&'static str> = Vec::with_capacity(values.len());
            let mut bad: Option<&str> = None;
            for v in &values {
                match vocab.iter().find(|k| k.eq_ignore_ascii_case(v)) {
                    Some(k) => canon.push(k),
                    None => {
                        bad = Some(v);
                        break;
                    }
                }
            }
            if let Some(bad) = bad {
                diagnostics.push(Diagnostic::warn(
                    &tok.raw,
                    format!(
                        "`{key}:` takes {} — `{bad}` searched as an ordinary word",
                        vocab.join("|")
                    ),
                    None,
                ));
                keep(&mut terms, &tok);
                continue;
            }
            canonical = canon;
            values = canonical.clone();
        }
        apply_filter(
            &mut Modifiers {
                filters: &mut filters,
                sort: &mut sort,
                explain: &mut explain,
                group: &mut group,
                facets: &mut facets,
            },
            spec,
            negated,
            &values,
        );
    }

    Extracted {
        terms,
        filters,
        sort,
        explain,
        group,
        facets,
        diagnostics,
    }
}

/// The mutable target [`apply_filter`] writes into — the typed view plus the
/// query MODIFIERS that are not filters (`sort:`/`explain:`/`group:`/
/// `facets:`). One struct rather than five `&mut` parameters: kbcq/1 grows by
/// design, and a sixth positional `&mut bool` is exactly how a caller ends up
/// passing them in the wrong order.
struct Modifiers<'a> {
    filters: &'a mut Filters,
    sort: &'a mut Option<SortKey>,
    explain: &'a mut bool,
    group: &'a mut Option<GroupKey>,
    facets: &'a mut bool,
}

/// Write one recognised, validated filter token into the typed view. The
/// ONLY place a `key` string turns into a field — every consumer reads the
/// field, never re-parses the token.
fn apply_filter(m: &mut Modifiers<'_>, spec: &FilterKeySpec, negated: bool, values: &[&str]) {
    let filters = &mut *m.filters;
    let owned = || values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
    let last = || values.last().copied().unwrap_or_default().to_string();
    match (spec.key, negated) {
        ("lang", false) => filters.lang = Some(last()),
        ("lang", true) => filters.not_lang.extend(owned()),
        ("path", false) => filters.path = Some(last()),
        ("path", true) => filters.not_path.extend(owned()),
        ("repo", _) => filters.repo = Some(last()),
        ("case", _) => filters.case = Some(matches!(last().as_str(), "yes" | "true" | "1")),
        ("ext", false) => filters.ext.extend(owned()),
        ("ext", true) => filters.not_ext.extend(owned()),
        ("kind", false) => filters.kind.extend(owned()),
        ("kind", true) => filters.not_kind.extend(owned()),
        ("sort", _) => {
            *m.sort = Some(match last().as_str() {
                "path" => SortKey::Path,
                _ => SortKey::Relevance,
            })
        }
        ("explain", _) => *m.explain = matches!(last().as_str(), "1" | "yes" | "true"),
        ("group", _) => {
            *m.group = Some(match last().as_str() {
                "file" => GroupKey::File,
                "kind" => GroupKey::Kind,
                "lane" => GroupKey::Lane,
                "dir" => GroupKey::Dir,
                _ => GroupKey::None,
            })
        }
        ("facets", _) => *m.facets = matches!(last().as_str(), "1" | "yes" | "true"),
        // Unreachable while `FILTER_SPECS` and this match agree — and
        // `every_declared_filter_key_is_applied` is what keeps them
        // agreeing.
        _ => {}
    }
}

/// The did-you-mean for an unknown key — the nearest [`FILTER_SPECS`] key
/// within [`SUGGEST_MAX_DISTANCE`] edits, or `None`.
fn suggest_key(key: &str) -> Option<&'static str> {
    let lower = key.to_lowercase();
    FILTER_SPECS
        .iter()
        .map(|s| (s.key, edit_distance(&lower, s.key)))
        .filter(|(_, d)| *d <= SUGGEST_MAX_DISTANCE)
        .min_by_key(|(k, d)| (*d, *k))
        .map(|(k, _)| k)
}

/// Plain Levenshtein — small strings only (a filter key), two rolling rows.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Parse the remainder after a leading `/` — see the module doc's
/// "`/pattern[/flags]`" section.
fn parse_text_prefix(rest: &str) -> ParsedQuery {
    let mut out = if let Some(close) = rest.find('/') {
        let pattern = &rest[..close];
        let after = &rest[close + 1..];
        let flags_end = after
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(after.len());
        let flags = &after[..flags_end];
        // Whatever follows the flags run (further trailing filter tokens, if
        // any) is filter-extracted; the resulting TERMS are discarded — the
        // delimited pattern is already fully specified and never gets extra
        // words appended to it.
        let mut e = extract(after[flags_end..].trim_start());
        if flags.contains('i') && e.filters.case.is_none() {
            e.filters.case = Some(false);
        }
        e.terms = vec![QueryTerm {
            text: pattern.to_string(),
            quoted: false,
        }];
        finish(vec![Lane::Text], e, Some(TextMode::Regex))
    } else {
        let e = extract(rest);
        let joined = join_terms(&e.terms);
        let mode = if grep_regex::RegexMatcherBuilder::new()
            .build(&joined)
            .is_ok()
        {
            TextMode::Regex
        } else {
            TextMode::Literal
        };
        finish(vec![Lane::Text], e, Some(mode))
    };
    out.normalized = normalize(&out);
    out
}

/// Re-render a parsed query canonically — see [`ParsedQuery::normalized`].
/// Terms first (in author order, quoting preserved), then filters in
/// [`FILTER_SPECS`] declaration order (positives before negatives), so two
/// queries that MEAN the same thing normalize to the same string.
pub fn normalize(p: &ParsedQuery) -> String {
    let mut out = String::new();
    let single = (p.lanes.len() == 1).then(|| p.lanes[0]);
    match single {
        Some(Lane::Text) => {
            out.push('/');
            out.push_str(&p.query);
            // Only a REGEX query normalizes to the delimited form. Closing
            // the slash on a LITERAL one would be a lie that re-parses
            // differently: the delimited form is always `TextMode::Regex`
            // (the module doc's "explicit signal"), so `/​*abc` — bare,
            // literal because the pattern does not compile — would come
            // back as an uncompilable regex. The bare form round-trips it
            // verbatim.
            if p.text_mode == TextMode::Regex {
                out.push('/');
                // The `i` flag and `case:no` are the same fact; render the
                // flag form, which is what a text query looks like when
                // typed. (A literal query has no flags slot, so its
                // `case:` falls through to the ordinary filter below.)
                if p.filters.case == Some(false) {
                    out.push('i');
                }
            }
        }
        Some(lane) => {
            if let Some(prefix) = lane.prefix() {
                out.push_str(prefix);
            }
            out.push_str(&render_terms(&p.terms));
        }
        None => out.push_str(&render_terms(&p.terms)),
    }

    let mut parts: Vec<String> = Vec::new();
    for spec in FILTER_SPECS {
        match spec.key {
            "lang" => {
                if let Some(v) = &p.filters.lang {
                    parts.push(format!("lang:{}", quote_if_needed(v)));
                }
                push_multi(&mut parts, "-lang", &p.filters.not_lang);
            }
            "path" => {
                if let Some(v) = &p.filters.path {
                    parts.push(format!("path:{}", quote_if_needed(v)));
                }
                push_multi(&mut parts, "-path", &p.filters.not_path);
            }
            "repo" => {
                if let Some(v) = &p.filters.repo {
                    parts.push(format!("repo:{}", quote_if_needed(v)));
                }
            }
            "case" => match (single, p.filters.case) {
                // Already rendered as the `/…/i` flag above — but ONLY the
                // delimited (regex) form has a flags slot.
                (Some(Lane::Text), Some(false)) if p.text_mode == TextMode::Regex => {}
                (_, Some(true)) => parts.push("case:yes".to_string()),
                (_, Some(false)) => parts.push("case:no".to_string()),
                (_, None) => {}
            },
            "ext" => {
                push_multi(&mut parts, "ext", &p.filters.ext);
                push_multi(&mut parts, "-ext", &p.filters.not_ext);
            }
            "kind" => {
                push_multi(&mut parts, "kind", &p.filters.kind);
                push_multi(&mut parts, "-kind", &p.filters.not_kind);
            }
            "sort" => match p.sort {
                Some(SortKey::Path) => parts.push("sort:path".to_string()),
                Some(SortKey::Relevance) => parts.push("sort:relevance".to_string()),
                None => {}
            },
            "explain" => {
                if p.explain {
                    parts.push("explain:1".to_string());
                }
            }
            "group" => {
                if let Some(g) = p.group {
                    parts.push(format!("group:{}", g.as_str()));
                }
            }
            "facets" => {
                if p.facets {
                    parts.push("facets:1".to_string());
                }
            }
            _ => {}
        }
    }
    for part in parts {
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&part);
    }
    out.trim().to_string()
}

fn push_multi(parts: &mut Vec<String>, key: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    let joined = values
        .iter()
        .map(|v| quote_if_needed(v))
        .collect::<Vec<_>>()
        .join("|");
    parts.push(format!("{key}:{joined}"));
}

fn render_terms(terms: &[QueryTerm]) -> String {
    terms
        .iter()
        .map(|t| {
            if t.quoted || t.text.contains(' ') {
                format!("\"{}\"", t.text)
            } else {
                t.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_if_needed(v: &str) -> String {
    if v.contains(' ') {
        format!("\"{v}\"")
    } else {
        v.to_string()
    }
}

/// Every filter key this grammar recognises, in declaration order —
/// exposed for this module's own tests and for `unified`'s explain surface.
pub fn filter_keys() -> Vec<&'static str> {
    FILTER_SPECS.iter().map(|s| s.key).collect()
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
        assert!(p.diagnostics.is_empty());
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
    fn case_filter_unrecognised_value_stays_a_literal_word_with_a_diagnostic() {
        let p = parse("case:closed widget");
        assert_eq!(p.filters.case, None);
        assert_eq!(p.query, "case:closed widget");
        assert_eq!(p.diagnostics.len(), 1);
        assert_eq!(p.diagnostics[0].token, "case:closed");
    }

    #[test]
    fn empty_filter_value_stays_a_literal_word() {
        let p = parse("widget lang:");
        assert_eq!(p.filters.lang, None);
        assert_eq!(p.query, "widget lang:");
        assert_eq!(p.diagnostics.len(), 1);
    }

    #[test]
    fn repeated_single_valued_key_last_one_wins() {
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

    // --- kbcq/1 additions (V71-D1) ------------------------------------

    #[test]
    fn a_quoted_phrase_is_one_term_and_is_never_filter_extracted() {
        let p = parse("\"lang:rust is a phrase\" widget");
        assert_eq!(p.filters.lang, None);
        assert_eq!(p.query, "lang:rust is a phrase widget");
        assert_eq!(p.terms.len(), 2);
        assert!(p.terms[0].quoted);
        assert!(!p.terms[1].quoted);
    }

    #[test]
    fn a_quoted_filter_value_keeps_its_spaces() {
        let p = parse("widget path:\"src/my dir\"");
        assert_eq!(p.filters.path.as_deref(), Some("src/my dir"));
        assert_eq!(p.query, "widget");
    }

    #[test]
    fn multi_valued_keys_take_alternation_and_accumulate() {
        let p = parse("order kind:class|module kind:method");
        assert_eq!(p.filters.kind, vec!["class", "module", "method"]);
        assert_eq!(p.query, "order");
    }

    #[test]
    fn negation_lands_in_the_matching_not_field() {
        let p = parse("order -path:spec/ -ext:min.js -lang:ruby -kind:const");
        assert_eq!(p.filters.not_path, vec!["spec/"]);
        assert_eq!(p.filters.not_ext, vec!["min.js"]);
        assert_eq!(p.filters.not_lang, vec!["ruby"]);
        assert_eq!(p.filters.not_kind, vec!["const"]);
        assert_eq!(p.query, "order");
        assert!(p.diagnostics.is_empty());
    }

    #[test]
    fn negating_a_non_negatable_key_is_a_diagnostic_not_a_silent_drop() {
        let p = parse("order -repo:kb");
        assert_eq!(p.filters.repo, None);
        assert_eq!(p.query, "order -repo:kb");
        assert_eq!(p.diagnostics.len(), 1);
        assert!(p.diagnostics[0].message.contains("cannot be negated"));
    }

    #[test]
    fn an_unknown_key_is_searched_literally_with_a_did_you_mean() {
        let p = parse("widget laang:rust");
        assert_eq!(p.query, "widget laang:rust");
        assert_eq!(p.diagnostics.len(), 1);
        assert_eq!(p.diagnostics[0].suggestion.as_deref(), Some("lang:rust"));
    }

    #[test]
    fn a_far_away_unknown_key_gets_no_suggestion_and_no_noise() {
        let p = parse("widget gorgonzola:yes");
        assert_eq!(p.query, "widget gorgonzola:yes");
        // Nothing within SUGGEST_MAX_DISTANCE — pointing at a random key
        // would be worse than saying nothing.
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    }

    #[test]
    fn a_url_or_a_rust_path_is_never_mistaken_for_a_filter() {
        assert_eq!(
            parse("https://example.com/x").query,
            "https://example.com/x"
        );
        assert_eq!(parse("@Foo::bar").query, "Foo::bar");
    }

    #[test]
    fn sort_and_explain_are_modifiers_not_filters() {
        let p = parse("order sort:path explain:1");
        assert_eq!(p.sort, Some(SortKey::Path));
        assert!(p.explain);
        assert_eq!(p.query, "order");
        let p2 = parse("order sort:churn");
        assert_eq!(p2.sort, None);
        assert_eq!(p2.diagnostics.len(), 1);
        assert_eq!(p2.query, "order sort:churn");
    }

    #[test]
    fn normalize_round_trips_as_a_fixed_point() {
        for raw in [
            "add widget",
            "@GitRepo::open",
            "#config.rs",
            "~fixed the gizmo race",
            "~~gorgonzola bug",
            "?nl how does auth work",
            "/fn add/",
            "/fn add/i",
            "/needle lang:rust",
            "/*abc",
            "/*abc case:no",
            "widget lang:rust path:src/ repo:kb case:yes",
            "widget case:YES sort:PATH explain:YES",
            "order kind:class|module -path:spec/ ext:rb sort:path explain:1",
            "\"a phrase\" widget",
            "widget laang:rust",
        ] {
            let once = parse(raw);
            let twice = parse(&once.normalized);
            assert_eq!(
                twice.normalized, once.normalized,
                "normalize is not a fixed point for {raw:?}"
            );
            assert_eq!(twice.lanes, once.lanes, "lanes drifted for {raw:?}");
            assert_eq!(twice.query, once.query, "query drifted for {raw:?}");
            // Load-bearing: normalizing a LITERAL text query into the
            // delimited form would silently turn it into a regex.
            assert_eq!(
                twice.text_mode, once.text_mode,
                "text_mode drifted for {raw:?}"
            );
            assert_eq!(twice.filters, once.filters, "filters drifted for {raw:?}");
            assert_eq!(twice.sort, once.sort, "sort drifted for {raw:?}");
            assert_eq!(twice.explain, once.explain, "explain drifted for {raw:?}");
        }
    }

    // --- the dead-surface walk ----------------------------------------

    /// V71-D1's answer to the v7.0 defect class (a declared surface with no
    /// handler): every key in [`FILTER_SPECS`] must (a) actually parse,
    /// (b) land in a typed field, and (c) be READ by the module its spec
    /// names. (c) is a source scan — the same shape as this crate's
    /// `tests/security/git_argv_lint.rs` gate — so declaring `kind:` and
    /// forgetting to filter on it fails HERE, loudly, naming the key,
    /// rather than shipping a filter that silently does nothing.
    #[test]
    fn every_declared_filter_key_has_a_consumer() {
        // Whitespace-stripped, because rustfmt breaks a long chain across
        // lines (`filters\n    .not_path\n    .iter()`) and a scan that
        // missed that would fail on FORMATTING rather than on wiring. Like
        // this crate's `git_argv_lint`, this is a scan with the limits of a
        // scan: it proves the expression appears in the consuming module,
        // not that it is reached at runtime.
        let unified: String = include_str!("unified.rs")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        for spec in FILTER_SPECS {
            assert_eq!(
                spec.consumer_module, "unified.rs",
                "{}: only unified.rs is scanned today — add the module to this test \
                 before pointing a spec at it",
                spec.key
            );
            assert!(
                unified.contains(spec.consumer_expr),
                "kbcq/1 declares `{}:` but {} never reads `{}` — either wire it or \
                 drop the key (a filter that parses and does nothing is the v7.0 \
                 dead-surface defect)",
                spec.key,
                spec.consumer_module,
                spec.consumer_expr,
            );
            if spec.negatable {
                let not_expr = format!("filters.not_{}", spec.key);
                assert!(
                    unified.contains(&not_expr),
                    "kbcq/1 declares `-{}:` as negatable but {} never reads `{}`",
                    spec.key,
                    spec.consumer_module,
                    not_expr,
                );
            }
        }
    }

    /// The other half of the same walk: a key that parses into NOTHING.
    /// Probes every declared key through the real parser and asserts the
    /// token was consumed (it left the query text) and landed somewhere —
    /// catching a key added to [`FILTER_SPECS`] but forgotten in
    /// [`apply_filter`]'s match.
    #[test]
    fn every_declared_filter_key_is_applied() {
        for spec in FILTER_SPECS {
            let value = spec.values.map_or("probe", |v| v[0]);
            let p = parse(&format!("needle {}:{value}", spec.key));
            assert_eq!(
                p.query, "needle",
                "`{}:` parsed but was not consumed — add it to `apply_filter`",
                spec.key
            );
            assert!(
                p.diagnostics.is_empty(),
                "`{}:{value}` should be a clean parse, got {:?}",
                spec.key,
                p.diagnostics
            );
            let touched = p.filters != Filters::default()
                || p.sort.is_some()
                || p.explain
                || p.group.is_some()
                || p.facets;
            assert!(
                touched,
                "`{}:` left every typed field at its default — `apply_filter` \
                 dropped it on the floor",
                spec.key
            );
        }
    }

    /// V71-D2 — the `group:` vocabulary's own dead-surface walk, parser
    /// half: every value [`GROUP_KEYS`] declares must actually parse to the
    /// variant it names, and [`GROUP_VALUES`] (what [`FILTER_SPECS`]
    /// validates against) must be exactly that set in the same order. A
    /// variant added to the enum but not to the vocabulary would be
    /// unreachable from the grammar; a string added to the vocabulary but
    /// not to `apply_filter`'s match would parse to `none` and silently
    /// ungroup the page. The other half of this walk lives in `unified`
    /// (`every_group_key_partitions_the_page`), against the GROUPER.
    #[test]
    fn every_declared_group_key_parses() {
        for g in GROUP_KEYS {
            let p = parse(&format!("needle group:{}", g.as_str()));
            assert_eq!(p.query, "needle", "`group:{}` was not consumed", g.as_str());
            assert!(p.diagnostics.is_empty(), "`group:{}` warned", g.as_str());
            assert_eq!(
                p.group,
                Some(g),
                "`group:{}` parsed to {:?} — add it to `apply_filter`",
                g.as_str(),
                p.group
            );
            // …and it survives a normalize round trip, so a saved search or
            // a facet-written query keeps its grouping.
            assert_eq!(parse(&p.normalized).group, Some(g));
        }
    }

    #[test]
    fn group_vocabulary_matches_the_group_keys() {
        let from_enum: Vec<&str> = GROUP_KEYS.iter().map(|g| g.as_str()).collect();
        assert_eq!(
            GROUP_VALUES.to_vec(),
            from_enum,
            "GROUP_VALUES must be GROUP_KEYS, in order"
        );
    }

    /// `group:none` is NOT the same fact as no `group:` token at all, and
    /// `normalize` must keep them apart — otherwise a saved "ungrouped"
    /// search silently re-acquires whatever the page defaults to.
    #[test]
    fn explicit_group_none_is_distinct_from_an_absent_group() {
        assert_eq!(parse("widget").group, None);
        assert_eq!(parse("widget").normalized, "widget");
        assert_eq!(parse("widget group:none").group, Some(GroupKey::None));
        assert_eq!(parse("widget group:none").normalized, "widget group:none");
    }

    #[test]
    fn facets_is_off_unless_asked_for_and_normalizes_canonically() {
        assert!(!parse("widget").facets);
        assert!(parse("widget facets:1").facets);
        assert!(parse("widget facets:YES").facets);
        assert_eq!(parse("widget facets:YES").normalized, "widget facets:1");
        // `facets:0` is an explicit OFF: it parses cleanly (no diagnostic)
        // and renders as nothing, exactly like `explain:0`.
        assert!(!parse("widget facets:0").facets);
        assert!(parse("widget facets:0").diagnostics.is_empty());
        assert_eq!(parse("widget facets:0").normalized, "widget");
    }

    #[test]
    fn a_bad_group_value_is_a_word_and_a_diagnostic_never_a_failure() {
        let p = parse("widget group:sideways");
        assert_eq!(p.group, None);
        assert_eq!(p.query, "widget group:sideways");
        assert_eq!(p.diagnostics.len(), 1);
        assert!(p.diagnostics[0].message.contains("file|kind|lane|dir|none"));
    }

    #[test]
    fn filter_keys_matches_the_documented_set() {
        assert_eq!(
            filter_keys(),
            vec![
                "lang", "path", "repo", "case", "ext", "kind", "sort", "explain", "group", "facets"
            ]
        );
    }

    // --- the shared TS/Rust golden ------------------------------------

    #[derive(Debug, serde::Deserialize)]
    struct GoldenFile {
        schema: String,
        cases: Vec<GoldenCase>,
    }

    #[derive(Debug, serde::Deserialize)]
    struct GoldenCase {
        raw: String,
        lanes: Vec<String>,
        query: String,
        text_mode: String,
        normalized: String,
        #[serde(default)]
        filters: Filters,
        #[serde(default)]
        sort: Option<SortKey>,
        #[serde(default)]
        explain: bool,
        #[serde(default)]
        group: Option<GroupKey>,
        #[serde(default)]
        facets: bool,
        #[serde(default)]
        diagnostics: Vec<Diagnostic>,
    }

    const GOLDEN: &str = include_str!("../../grammar/kbcq.golden.json");

    /// One fixture, two parsers (this one and `web-code/src/lib/kbcq.ts`) —
    /// the lock-step root CLAUDE.md #29/#35 record for every grammar with a
    /// TS mirror. A change here that isn't mirrored there fails on the SPA
    /// side (`kbcq.golden.test.ts`) reading the SAME bytes.
    #[test]
    fn golden_corpus_matches_the_rust_parser() {
        let golden: GoldenFile = serde_json::from_str(GOLDEN).expect("kbcq.golden.json parses");
        assert_eq!(golden.schema, "kbcq/1");
        assert!(!golden.cases.is_empty());
        for case in &golden.cases {
            let p = parse(&case.raw);
            let lanes: Vec<String> = p.lanes.iter().map(|l| l.as_str().to_string()).collect();
            assert_eq!(lanes, case.lanes, "lanes for {:?}", case.raw);
            assert_eq!(p.query, case.query, "query for {:?}", case.raw);
            assert_eq!(
                serde_json::to_value(p.text_mode).unwrap(),
                serde_json::Value::String(case.text_mode.clone()),
                "text_mode for {:?}",
                case.raw
            );
            assert_eq!(p.filters, case.filters, "filters for {:?}", case.raw);
            assert_eq!(p.sort, case.sort, "sort for {:?}", case.raw);
            assert_eq!(p.explain, case.explain, "explain for {:?}", case.raw);
            assert_eq!(p.group, case.group, "group for {:?}", case.raw);
            assert_eq!(p.facets, case.facets, "facets for {:?}", case.raw);
            assert_eq!(
                p.diagnostics, case.diagnostics,
                "diagnostics for {:?}",
                case.raw
            );
            assert_eq!(
                p.normalized, case.normalized,
                "normalized for {:?}",
                case.raw
            );
        }
    }

    /// The golden is also the grammar's COVERAGE contract: every lane
    /// prefix and every declared filter key must appear in at least one
    /// case, so the TS mirror can never be "green" on a grammar half of
    /// which it has never seen a case.
    #[test]
    fn golden_corpus_covers_every_lane_and_every_filter_key() {
        let golden: GoldenFile = serde_json::from_str(GOLDEN).unwrap();
        for lane in LANE_ORDER {
            assert!(
                golden
                    .cases
                    .iter()
                    .any(|c| c.lanes == vec![lane.as_str().to_string()]),
                "no golden case selects the {} lane alone",
                lane.as_str()
            );
        }
        for spec in FILTER_SPECS {
            let needle = format!("{}:", spec.key);
            assert!(
                golden.cases.iter().any(|c| c.raw.contains(&needle)),
                "no golden case exercises `{}:`",
                spec.key
            );
        }
    }
}
