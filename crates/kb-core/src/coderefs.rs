//! Code references — the doc→code half of the DCB bridge (invariant #2).
//!
//! Pure, LLM-free, corpus-local, golden-pinned. Sibling of [`crate::links`]
//! in every way that matters: no I/O, no clock, no storage, deterministic
//! given the bytes. The ONE structural rule this module exists to enforce is
//! that kb can never mint a trust class — every output field is a *hint*
//! (`path_hint`, `line_start`, `symbol_container`), because kb has no working
//! tree and no symbol index. Existence, ambiguity and line drift are decided
//! by kb-code's `codelens/1` (see the DCB v1 plan, §Stage 2 — kept outside this repo)
//! and are NEVER persisted here.
//!
//! ### Input surface
//!
//! [`extract`] takes `EnrichCtx::html` — the PARSED document body, so a
//! Markdown note's backtick spans arrive as real `<code>` elements and one
//! grammar covers both artifact kinds. The `script` / `style` / `template` /
//! `noscript` subtrees are skipped exactly as [`crate::parser`]'s
//! `TEXT_SKIP_TAGS` skips them, which is what keeps the
//! `<template id="kb-prompt">` bundle (invariant #5) out of the ref set:
//! html5ever puts template children in the main tree, so they ARE reachable
//! from a descendant walk and the skip is load-bearing, not decorative.
//!
//! ### The closed grammar
//!
//! * whitelisted-extension paths ([`CODE_EXTENSIONS`]), optionally suffixed
//!   `:LINE`, `:A-B` or `:A,B-C,D`;
//! * `path#member` (a path plus a Ruby-ish member name);
//! * `Namespace::Class` — the `::` is REQUIRED, a bare CapWord is never a
//!   symbol;
//! * `Class#method` / `Namespace::Class#method`;
//! * gem/vendor paths (`name-1.2.3/…`, `node_modules/…`) ⇒ `external`, kept
//!   verbatim and never resolved against an app repo;
//! * GitHub issues, from `<a href>` only — never from prose.
//!
//! Everything else is refused, loudly and on purpose ([`HARD_REJECT_SUBSTRINGS`],
//! [`DOC_EXTENSIONS`], the directory-ref refusal in [`parse_path_token`]).

use crate::headings::heading_id;
use crate::parser::{has_skip_ancestor, BLOCK_TAGS, TEXT_SKIP_TAGS};
use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use std::sync::{LazyLock, OnceLock};

// --- Constants ---------------------------------------------------------------

/// The ONLY file extensions a path production may end in. Longest suffix wins,
/// so `.js.erb` beats `.erb` and `.html.erb` beats `.erb`. Derived empirically
/// from the motivating corpus plus the code languages kb-code itself indexes.
///
/// Deliberate EXCLUSIONS, each with a measured reason:
///   `sql`   — `Arel.sql` is a Ruby method call and the motivating corpus has
///             zero real `.sql` refs; it was the single worst collision in the
///             prototype.
///   `env`   — `request.env` / `process.env`; a dotfile is never a citation.
///   `config`— `jest.config` / `vitest.config` are tool nouns, not files.
///   `html`, `htm`, `md`, `markdown` — see [`DOC_EXTENSIONS`].
///
/// Matching is ASCII-case-SENSITIVE (every entry is lowercase): a corpus that
/// writes `Foo.RB` is not a thing, and case folding would let `Time.Zone.Go`
/// shapes in.
pub const CODE_EXTENSIONS: &[&str] = &[
    // compound (matching is longest-suffix, so these win over their own tails)
    "js.erb",
    "html.erb",
    "json.erb",
    "css.erb",
    "turbo_stream.erb",
    // ruby / rails
    "rb",
    "rake",
    "gemspec",
    "erb",
    "haml",
    "slim",
    // js / ts
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "tsx",
    "vue",
    "svelte",
    // other languages
    "py",
    "go",
    "rs",
    "java",
    "kt",
    "swift",
    "php",
    "c",
    "h",
    "cpp",
    "hpp",
    // shells
    "sh",
    "fish",
    "ps1",
    // data / config / infra
    "yml",
    "yaml",
    "toml",
    "json",
    "xml",
    "proto",
    "graphql",
    "conf",
    "ini",
    "lock",
    "tf",
    // styles + assets
    "css",
    "scss",
    "sass",
    "less",
    "svg",
    "txt",
];

/// Extensions that name a *kb artifact*, never a code file. A `<code>` token
/// ending in one of these is HARD-REJECTED even when it carries a `:line`
/// suffix (`plan-a2-conversion.html:390` is a sibling doc, not a source
/// location). doc↔doc navigation already has two homes — `<a href>` edges and
/// wikilinks (invariant #29) — and duplicating it here would mint corpus-wide
/// `absent` misses against the code repo. Escape hatch: `data-kb-ref`.
///
/// No runtime check is needed — those strings are simply absent from
/// [`CODE_EXTENSIONS`], so [`longest_code_ext`] returns `None`. The const
/// exists as the documented refusal, asserted against `CODE_EXTENSIONS` in
/// `doc_extensions_are_not_code_extensions`.
pub const DOC_EXTENSIONS: &[&str] = &["html", "htm", "md", "markdown"];

/// Substrings that disqualify a candidate outright. Applied to the DECODED
/// text (`scraper`'s `.text()` already decodes, so a source `=&gt;` is seen
/// here as `=>`; a raw-bytes scan would refuse it only incidentally, on the `&`).
///
/// `!` and `?` are deliberately ABSENT — legal Ruby method suffixes
/// (`Products::Indexer#indexable?`, `Order#confirm_payment!`) that the member
/// production constrains anyway. `#` is absent because it is load-bearing for
/// `Class#method` and `path#member`. `,` is absent because it is both a
/// Pass-B delimiter and part of the `path:1,2,3` line-list.
pub const HARD_REJECT_SUBSTRINGS: &[&str] = &[
    "(", ")", "{", "}", "[", "]", // calls, hashes, arrays, brace expansion
    "$", "@", // globals / ivars / npm scopes / URLs
    "\"", "'", "`", // string literals
    "#{", "->", "=>", // interpolation, lambda, hash rocket
    "<", ">", // `mlf-<id>`, `<uuid>`, generics, markup
    "*", "|", "\\", "%", "&", "^", "~", ";",        // globs, regex, shell, SQL, ops
    "..",       // Ruby ranges AND path traversal
    "\u{2026}", // `…` elision (`db/migrate/2024…_x.rb`)
    "://",      // URLs
];

/// Pass B (token fallback) is skipped for elements larger than this. The
/// motivating corpus's largest `<pre><code>` listing is ~4 KB; 16 KiB leaves
/// generous headroom while bounding the token scan for a doc that pastes a
/// whole vendored file.
pub const TOKEN_SCAN_MAX_BYTES: usize = 16 * 1024;

/// Hard cap on refs emitted per document. Measured max on the motivating
/// corpus: 91. A doc that trips this stores the first N and sets
/// [`Extraction::truncated`] — honest, never a silent drop.
pub const MAX_REFS_PER_DOC: usize = 2_000;

/// Human-facing context: the enclosing block's plain text, whitespace-
/// collapsed, truncated to this many BYTES on a char boundary. Rendered as
/// the "why this ref is here" line. NEVER an input to any resolution
/// predicate.
pub const CONTEXT_MAX_BYTES: usize = 200;

/// Machine-facing context: ordered, deduped, identifier-shaped tokens; THE
/// ONLY context input `codelens/1`'s line_state predicate may read.
pub const CONTEXT_TOKENS_MAX: usize = 8;
/// Byte budget for the space-joined [`CodeRef::context_tokens`].
pub const CONTEXT_TOKENS_MAX_BYTES: usize = 120;
/// Minimum token length (after stripping one trailing `!`/`?`).
pub const CONTEXT_TOKEN_MIN_LEN: usize = 4;

/// Elements whose text contributes machine-facing context tokens: the author
/// already marked the code-ish words, so no prose stoplist is needed.
const CONTEXT_TOKEN_TAGS: &[&str] = &["code", "kbd", "samp", "var"];

/// How far above/below a `<pre>` ref's own line the context harvest reaches.
const PRE_CONTEXT_LINE_RADIUS: usize = 2;

// --- Regex set ---------------------------------------------------------------

macro_rules! rx {
    ($name:ident, $pat:literal) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

rx!(RE_LINES, r"^\d+(-\d+)?(,\d+(-\d+)?)*$");
rx!(RE_FIRST_SEG, r"^[A-Za-z0-9_][A-Za-z0-9_+-]*$");
rx!(RE_MID_SEG, r"^[A-Za-z0-9_][A-Za-z0-9_.+-]*$");
rx!(RE_BASENAME, r"^[A-Za-z0-9_][A-Za-z0-9_.+-]*$");
rx!(RE_GEM_PREFIX, r"^[a-z0-9_]+(-[a-z0-9_]+)*-\d+(\.\d+)+/");
rx!(RE_MEMBER, r"^[a-z_][A-Za-z0-9_]*[!?]?$");
rx!(
    RE_SYMBOL_CONST,
    r"^[A-Z][A-Za-z0-9_]*(::[A-Z][A-Za-z0-9_]*)+$"
);
rx!(
    RE_SYMBOL_METHOD,
    r"^([A-Z][A-Za-z0-9_]*(?:::[A-Z][A-Za-z0-9_]*)*)#([a-z_][A-Za-z0-9_]*[!?]?)$"
);
rx!(RE_CONTEXT_TOKEN, r"^[A-Za-z_][A-Za-z0-9_]*[!?]?$");
rx!(
    RE_ISSUE_HREF,
    r"(?i)^https?://(?:www\.)?github\.com/([A-Za-z0-9._-]+)/([A-Za-z0-9._-]+)/issues/(\d+)/?([?#].*)?$"
);
rx!(
    RE_CODE_REV,
    r"^([A-Za-z0-9._-]+)@([0-9a-f]{7,40})(\+dirty)?$"
);
rx!(RE_DECLARED_LINES, r"^(\d+)(?:-L?(\d+))?$");
// Pass-B token delimiters. Deliberately NOT `,` — that would shred
// `checkout_controller.rb:425,440`.
rx!(RE_TOKEN_SPLIT, "[\\s;()\\[\\]{}<>\"'`]+");
// Non-identifier runs, for the `<pre>` ±2-line context harvest.
rx!(RE_IDENT_SPLIT, r"[^A-Za-z0-9_!?]+");

// --- Types -------------------------------------------------------------------

/// The closed set of reference kinds. Wire/DB spelling is [`Self::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodeRefKind {
    Path,
    PathLine,
    PathRange,
    PathList,
    SymbolMethod,
    SymbolConst,
    Issue,
    External,
}

impl CodeRefKind {
    /// Wire/DB spelling: `path` | `path_line` | `path_range` | `path_list`
    /// | `symbol_method` | `symbol_const` | `issue` | `external`.
    pub fn as_str(&self) -> &'static str {
        match self {
            CodeRefKind::Path => "path",
            CodeRefKind::PathLine => "path_line",
            CodeRefKind::PathRange => "path_range",
            CodeRefKind::PathList => "path_list",
            CodeRefKind::SymbolMethod => "symbol_method",
            CodeRefKind::SymbolConst => "symbol_const",
            CodeRefKind::Issue => "issue",
            CodeRefKind::External => "external",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for anything outside the closed set.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "path" => CodeRefKind::Path,
            "path_line" => CodeRefKind::PathLine,
            "path_range" => CodeRefKind::PathRange,
            "path_list" => CodeRefKind::PathList,
            "symbol_method" => CodeRefKind::SymbolMethod,
            "symbol_const" => CodeRefKind::SymbolConst,
            "issue" => CodeRefKind::Issue,
            "external" => CodeRefKind::External,
            _ => return None,
        })
    }
}

/// One extracted reference, exactly as it will be stored and served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRef {
    /// 0-based, document order.
    pub ordinal: u32,
    pub kind: CodeRefKind,
    /// The token/element text VERBATIM, as the doc wrote it.
    ///
    /// (R11) For `kind == Issue`, this is whichever occurrence's literal
    /// href happened to win the dedup — it may be the bare issue URL or a
    /// `#issuecomment-…`-suffixed variant, depending on document order.
    /// Consumers must NEVER reconstruct the issue href from `raw_text`;
    /// rebuild it from `path_hint` (`"<owner>/<repo>"`) + `line_start` (the
    /// issue number) instead, e.g.
    /// `https://github.com/{path_hint}/issues/{line_start}`.
    pub raw_text: String,
    /// Repo-relative-ish path AS WRITTEN. Never normalized, never guessed.
    /// For `issue`, `"<owner>/<repo>"`.
    pub path_hint: Option<String>,
    /// For `issue`, the issue NUMBER (no extra column exists for it).
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    /// Normalized full span list (`"425,440"`, `"30,51-65,113-119"`) whenever
    /// the `:LINES` suffix was a comma list — set on `path_list`, but ALSO on
    /// an `external` ref carrying a comma list (`kind = External` wins over
    /// the line-shape classification for a gem/`node_modules` path, e.g.
    /// `gem-1.0/lib/a.rb:5,9`; `line_spans` still reflects the full list). Not
    /// present for `path_line`/`path_range`, whose single span already lives
    /// entirely in `line_start`/`line_end`. `line_start`/`line_end` always
    /// mirror the FIRST span when this is `Some`.
    pub line_spans: Option<String>,
    pub symbol_container: Option<String>,
    pub symbol_member: Option<String>,
    /// ≤ [`CONTEXT_MAX_BYTES`], may be empty. HUMAN-FACING ONLY.
    pub context: String,
    /// ≤ [`CONTEXT_TOKENS_MAX`] identifier-shaped tokens.
    pub context_tokens: Vec<String>,
    /// `true` when the ref came from `<code data-kb-ref="…">`.
    pub declared: bool,
    /// Index into [`Extraction::groups`]; `None` = before the first `h2`/`h3`
    /// (see [`Extraction::ungrouped_count`] — there is NO sentinel group).
    pub group: Option<usize>,
}

/// A ref-bearing `h2`/`h3` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRefGroup {
    /// Index among REF-BEARING groups, document order (== its index in
    /// [`Extraction::groups`]).
    pub ordinal: u32,
    /// == [`Self::anchor`] today; a separate field on purpose (the SPA's
    /// `kb:scroll-to-id` consumer wants a field named for what it is).
    pub key: String,
    pub label: String,
    pub anchor: String,
}

/// `<meta name="kb-code-rev" content="<label>@<sha>[+dirty]">`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeRev {
    pub label: String,
    pub sha: String,
    pub dirty: bool,
}

impl CodeRev {
    /// Round-trip spelling stored in `code_refs_docs.code_rev` and served on
    /// the wire. `parse_code_rev(&x.to_wire()) == Some(x)` is a unit test.
    pub fn to_wire(&self) -> String {
        if self.dirty {
            format!("{}@{}+dirty", self.label, self.sha)
        } else {
            format!("{}@{}", self.label, self.sha)
        }
    }
}

/// The whole per-document result. `Default` = "scanned, found nothing".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extraction {
    pub refs: Vec<CodeRef>,
    /// Only groups that own ≥1 ref. **No sentinel entry for ungrouped refs.**
    pub groups: Vec<CodeRefGroup>,
    /// Refs with `group == None` (before the first `h2`/`h3`). A consumer
    /// renders its own trailer from this scalar.
    pub ungrouped_count: u32,
    pub code_rev: Option<CodeRev>,
    /// Hit [`MAX_REFS_PER_DOC`].
    pub truncated: bool,
}

/// A parsed path token (the §2.3 + §2.4 productions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPath {
    pub kind: CodeRefKind,
    pub path_hint: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub line_spans: Option<String>,
    /// `Some` only for the combined `path#member` production.
    pub symbol_member: Option<String>,
}

/// A parsed symbol token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSymbol {
    pub kind: CodeRefKind,
    pub container: String,
    pub member: Option<String>,
}

/// `https://github.com/{owner}/{repo}/issues/{number}`, query + fragment
/// stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub owner: String,
    pub repo: String,
    pub number: u32,
}

/// The `data-kb-ref` attribute grammar's output. TOTAL — a malformed value is
/// still returned (`well_formed = false`) so a typo renders loudly as
/// "declared but absent" instead of vanishing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredRef {
    pub kind: CodeRefKind,
    pub path_hint: String,
    pub line_start: Option<u32>,
    pub line_end: Option<u32>,
    pub well_formed: bool,
}

// --- Pure helpers ------------------------------------------------------------

/// Longest whitelisted extension suffix of `body`, without the dot.
pub fn longest_code_ext(body: &str) -> Option<&'static str> {
    let mut best: Option<&'static str> = None;
    for ext in CODE_EXTENSIONS {
        if body.len() > ext.len() + 1
            && body.as_bytes()[body.len() - ext.len() - 1] == b'.'
            && body.ends_with(ext)
            && best.is_none_or(|b| ext.len() > b.len())
        {
            best = Some(ext);
        }
    }
    best
}

/// True when any [`HARD_REJECT_SUBSTRINGS`] entry occurs in the DECODED text.
pub fn is_hard_rejected(text: &str) -> bool {
    HARD_REJECT_SUBSTRINGS.iter().any(|s| text.contains(s))
}

/// The §6 machine-context token filter, exposed so downstream lints and the
/// tests share one rule.
pub fn is_context_token(tok: &str) -> bool {
    if !RE_CONTEXT_TOKEN.is_match(tok) {
        return false;
    }
    let stem = tok.trim_end_matches(['!', '?']);
    stem.len() >= CONTEXT_TOKEN_MIN_LEN && !stem.chars().all(|c| c.is_ascii_digit())
}

/// The §2.3 path production plus the §2.4 combined `path#member` form.
/// `None` when the token is not a path ref.
pub fn parse_path_token(token: &str) -> Option<ParsedPath> {
    if token.is_empty() || is_hard_rejected(token) {
        return None;
    }
    // `path#member` — a path (kind `path` ONLY) plus a Ruby-ish member. Split
    // first: the member's `#` is never legal inside a path segment.
    if let Some((left, right)) = token.split_once('#') {
        if right.contains('#') || !RE_MEMBER.is_match(right) {
            return None;
        }
        let base = parse_pure_path(left)?;
        if base.kind != CodeRefKind::Path {
            return None;
        }
        return Some(ParsedPath {
            symbol_member: Some(right.to_string()),
            ..base
        });
    }
    parse_pure_path(token)
}

/// The §2.3 production without the `#member` tail.
fn parse_pure_path(token: &str) -> Option<ParsedPath> {
    if token.is_empty() || is_hard_rejected(token) {
        return None;
    }
    // 1. Optional trailing `:LINES`, split on the LAST `:`.
    let (body, lines) = match token.rsplit_once(':') {
        Some((head, tail)) if !head.is_empty() && RE_LINES.is_match(tail) => (head, Some(tail)),
        _ => (token, None),
    };
    // 2. Directory refs are REJECTED (a repo dir has no landing surface in
    //    kb-code's per-FILE reader, so it would render as doc-rot).
    if body.starts_with('/') || body.ends_with('/') {
        return None;
    }
    // 3. Whitelisted extension, longest suffix wins.
    let ext = longest_code_ext(body)?;
    // 4. Segments.
    let segs: Vec<&str> = body.split('/').collect();
    if segs.iter().any(|s| s.is_empty()) {
        return None;
    }
    let external = RE_GEM_PREFIX.is_match(body) || segs[0] == "node_modules";
    if segs.len() > 1 && !external {
        if !RE_FIRST_SEG.is_match(segs[0]) {
            return None;
        }
        if segs[1..segs.len() - 1]
            .iter()
            .any(|s| !RE_MID_SEG.is_match(s))
        {
            return None;
        }
    }
    // 5. Basename with a non-empty stem.
    let base = segs[segs.len() - 1];
    if !RE_BASENAME.is_match(base) || base.len() <= ext.len() + 1 {
        return None;
    }
    // W1.gate (zero-risk grammar tightening): refuse when the basename minus
    // its extension(s) is empty — i.e. the WHOLE basename is itself one of
    // `CODE_EXTENSIONS`'s own entries, most notably a compound one
    // (`html.erb`, `js.erb`, `json.erb`, `css.erb`, `turbo_stream.erb`). The
    // check above only verifies a non-empty stem against the longest-suffix
    // match (`erb` for `html.erb`), so bare `html.erb` slips through as stem
    // `html` + ext `erb` — but `html.erb` is ALSO a `CODE_EXTENSIONS` entry
    // in its own right, and no real file is ever named exactly that; the
    // gate measured it ×6 as author shorthand for "an .html.erb file", never
    // a citation. `foo.html.erb` (a genuine stem) is unaffected.
    if CODE_EXTENSIONS.contains(&base) {
        return None;
    }

    let (kind, line_start, line_end, line_spans) = match lines {
        None => (
            if external {
                CodeRefKind::External
            } else {
                CodeRefKind::Path
            },
            None,
            None,
            None,
        ),
        Some(spans) => match first_span(spans) {
            Some((start, end)) => {
                let kind = if external {
                    CodeRefKind::External
                } else if spans.contains(',') {
                    CodeRefKind::PathList
                } else if spans.contains('-') {
                    CodeRefKind::PathRange
                } else {
                    CodeRefKind::PathLine
                };
                let list = if spans.contains(',') {
                    Some(spans.to_string())
                } else {
                    None
                };
                (kind, Some(start), end, list)
            }
            // A line number overflowed `u32` (e.g. `foo.rb:12345678901`, or a
            // range whose end overflows, `foo.rb:5-99999999999`). Loud
            // refusal: drop the `:LINES` suffix entirely rather than store an
            // invented hint (`line_start = Some(0)`) or a collapsed range —
            // the path itself is still a legitimate ref and survives.
            None => (
                if external {
                    CodeRefKind::External
                } else {
                    CodeRefKind::Path
                },
                None,
                None,
                None,
            ),
        },
    };

    Some(ParsedPath {
        kind,
        path_hint: body.to_string(),
        line_start,
        line_end,
        line_spans,
        symbol_member: None,
    })
}

/// First `(start, end)` of a `LINES` list; `end` is `None` for a bare number.
/// `None` on ANY line-number parse failure (an overflowing `u32`, e.g.
/// `12345678901`) — the caller treats that as "no line suffix" rather than
/// storing a wrong hint. A prior `unwrap_or(0)` here stored `line_start =
/// Some(0)` for an overflowing bare number, and collapsed an overflowing
/// range end back onto the (still-valid) start — both invented hints no
/// consumer asked for.
///
/// [B2] `None` ALSO for a zero or backwards span: `start == 0` (`foo.rb:0`
/// — there is no line 0, 1-based same as everywhere else in doc-lens),
/// `end == Some(0)`, or `end < start` (`foo.rb:5-3`). These parse as valid
/// `u32`s, so the overflow guard above never sees them — but they're the
/// same class of malformed hint the overflow case exists to refuse, and get
/// the identical treatment: drop the whole `:LINES` suffix, keep the path.
fn first_span(spans: &str) -> Option<(u32, Option<u32>)> {
    let first = spans.split(',').next().unwrap_or(spans);
    let (start, end) = match first.split_once('-') {
        Some((a, b)) => (a.parse().ok()?, Some(b.parse::<u32>().ok()?)),
        None => (first.parse().ok()?, None),
    };
    if start == 0 || end == Some(0) || end.is_some_and(|e| e < start) {
        return None;
    }
    Some((start, end))
}

/// The §2.5 symbol productions. `::` is MANDATORY for a const; a
/// `Class#method`'s left-hand side may be a bare CapWord.
pub fn parse_symbol_token(token: &str) -> Option<ParsedSymbol> {
    if token.is_empty() || is_hard_rejected(token) {
        return None;
    }
    if let Some(c) = RE_SYMBOL_METHOD.captures(token) {
        return Some(ParsedSymbol {
            kind: CodeRefKind::SymbolMethod,
            container: c[1].to_string(),
            member: Some(c[2].to_string()),
        });
    }
    if RE_SYMBOL_CONST.is_match(token) {
        return Some(ParsedSymbol {
            kind: CodeRefKind::SymbolConst,
            container: token.to_string(),
            member: None,
        });
    }
    None
}

/// The §2.6 issue-href production. Any scheme/host case; a trailing `/`,
/// `?query` and `#fragment` are all stripped, so
/// `…/issues/15351#issuecomment-5215243676` yields issue 15351.
///
/// (R11) `owner`/`repo`/`number` are the ONLY safe basis for a consumer to
/// mint an outbound issue link — see the [`CodeRef::raw_text`] doc comment
/// for why the literal href a `CodeRef` stores is not.
pub fn parse_issue_href(href: &str) -> Option<IssueRef> {
    let c = RE_ISSUE_HREF.captures(href.trim())?;
    Some(IssueRef {
        owner: c[1].to_string(),
        repo: c[2].to_string(),
        number: c[3].parse().ok()?,
    })
}

/// The §2.7 `data-kb-ref` attribute grammar: `PATH ( "#L" NUM ( "-" "L"? NUM )? )?`.
/// TOTAL — never fails. A value that fails the grammar comes back as
/// `kind = Path`, `path_hint` = the raw value, `well_formed = false`, and is
/// emitted anyway: a typo'd declared ref must render loudly, never silently
/// vanish.
pub fn parse_declared_ref(attr: &str) -> DeclaredRef {
    let attr = attr.trim();
    let malformed = || DeclaredRef {
        kind: CodeRefKind::Path,
        path_hint: attr.to_string(),
        line_start: None,
        line_end: None,
        well_formed: false,
    };
    let (path_part, line_part) = match attr.split_once("#L") {
        Some((p, l)) => (p, Some(l)),
        None => (attr, None),
    };
    let (line_start, line_end, lines_ok) = match line_part {
        None => (None, None, true),
        Some(spec) => match RE_DECLARED_LINES.captures(spec) {
            Some(c) => (
                c[1].parse::<u32>().ok(),
                c.get(2).and_then(|m| m.as_str().parse::<u32>().ok()),
                true,
            ),
            None => (None, None, false),
        },
    };
    if !lines_ok {
        return malformed();
    }
    let Some(parsed) = parse_pure_path(path_part) else {
        return malformed();
    };
    // The `#L` spec wins over any `:LINES` the path itself carried.
    let (start, end) = if line_part.is_some() {
        (line_start, line_end)
    } else {
        (parsed.line_start, parsed.line_end)
    };
    DeclaredRef {
        kind: parsed.kind,
        path_hint: parsed.path_hint,
        line_start: start,
        line_end: end,
        well_formed: true,
    }
}

/// The §2.8 `<meta name="kb-code-rev">` grammar. Malformed ⇒ `None`.
pub fn parse_code_rev(content: &str) -> Option<CodeRev> {
    let c = RE_CODE_REV.captures(content.trim())?;
    Some(CodeRev {
        label: c[1].to_string(),
        sha: c[2].to_string(),
        dirty: c.get(3).is_some(),
    })
}

// --- The DOM walk ------------------------------------------------------------

struct CodeRefSelectors {
    body: Selector,
    meta_code_rev: Selector,
}

impl CodeRefSelectors {
    fn instance() -> &'static Self {
        static SET: OnceLock<CodeRefSelectors> = OnceLock::new();
        SET.get_or_init(|| CodeRefSelectors {
            body: Selector::parse("body").unwrap(),
            meta_code_rev: Selector::parse(r#"meta[name="kb-code-rev"]"#).unwrap(),
        })
    }
}

/// A group that has been opened by an `h2`/`h3` but has not yet earned a slot
/// in [`Extraction::groups`] (it only earns one by owning a ref).
struct PendingGroup {
    key: String,
    label: String,
    anchor: String,
    /// Index in `Extraction::groups`, once materialised.
    idx: Option<usize>,
}

/// Extract every code reference from a parsed artifact body. `html` is
/// `EnrichCtx::html` (rendered markdown included). Deterministic; no I/O; no
/// clock. All output is owned, so the caller can drop the `!Send` DOM before
/// awaiting storage.
pub fn extract(html: &str) -> Extraction {
    let doc = Html::parse_document(html);
    let sels = CodeRefSelectors::instance();
    let mut out = Extraction {
        code_rev: doc
            .select(&sels.meta_code_rev)
            .next()
            .and_then(|e| e.value().attr("content"))
            .and_then(parse_code_rev),
        ..Default::default()
    };

    let root: ego_tree::NodeRef<'_, Node> = match doc.select(&sels.body).next() {
        Some(body) => *body,
        None => *doc.root_element(),
    };

    let mut heading_ordinal: usize = 0;
    let mut group: Option<PendingGroup> = None;
    let mut seen_issues: std::collections::HashSet<(String, String, u32)> =
        std::collections::HashSet::new();

    'walk: for node in root.descendants() {
        let Some(el) = node.value().as_element() else {
            continue;
        };
        let name = el.name();
        let is_heading = matches!(name, "h1" | "h2" | "h3");
        if !is_heading && name != "code" && name != "a" {
            continue;
        }
        if has_skip_ancestor(node, TEXT_SKIP_TAGS) {
            continue;
        }
        let Some(eref) = ElementRef::wrap(node) else {
            continue;
        };

        if is_heading {
            let label = collapse_ws(&eref.text().collect::<String>());
            if label.is_empty() {
                // The runtime skips blank headings AND does not advance its
                // ordinal for them (`headings.rs`).
                continue;
            }
            let anchor = el
                .attr("id")
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| heading_id(&label, heading_ordinal));
            heading_ordinal += 1;
            if name == "h2" || name == "h3" {
                // Opened on ENTER, so a ref inside the heading itself (a
                // GitHub issue link, say) belongs to this heading's group.
                group = Some(PendingGroup {
                    key: anchor.clone(),
                    label,
                    anchor,
                    idx: None,
                });
            }
            continue;
        }

        if name == "a" {
            let Some(href) = el.attr("href") else {
                continue;
            };
            let Some(issue) = parse_issue_href(href) else {
                continue;
            };
            if !seen_issues.insert((issue.owner.clone(), issue.repo.clone(), issue.number)) {
                continue; // first occurrence's ordinal/group win
            }
            let ctx = ElementContext::for_element(node, None);
            let tokens = ctx.tokens_for(None, None, None);
            let pushed = push_ref(
                &mut out,
                &mut group,
                CodeRef {
                    ordinal: 0,
                    kind: CodeRefKind::Issue,
                    raw_text: href.to_string(),
                    path_hint: Some(format!("{}/{}", issue.owner, issue.repo)),
                    line_start: Some(issue.number),
                    line_end: None,
                    line_spans: None,
                    symbol_container: None,
                    symbol_member: None,
                    context: ctx.context.clone(),
                    context_tokens: tokens,
                    declared: false,
                    group: None,
                },
            );
            if !pushed {
                break 'walk;
            }
            continue;
        }

        // `<code>`. A nested `<code>` inside another would double-emit the
        // same tokens; the outer element already owns them.
        if has_skip_ancestor(node, &["code"]) {
            continue;
        }

        // Declared refs bypass text inference entirely.
        if let Some(attr) = el.attr("data-kb-ref") {
            let d = parse_declared_ref(attr);
            let ctx = ElementContext::for_element(node, None);
            let stem = d
                .path_hint
                .rsplit('/')
                .next()
                .and_then(|b| b.split('.').next())
                .map(str::to_string);
            let tokens = ctx.tokens_for(None, None, stem.as_deref());
            let pushed = push_ref(
                &mut out,
                &mut group,
                CodeRef {
                    ordinal: 0,
                    kind: d.kind,
                    raw_text: attr.to_string(),
                    path_hint: Some(d.path_hint),
                    line_start: d.line_start,
                    line_end: d.line_end,
                    line_spans: None,
                    symbol_container: None,
                    symbol_member: None,
                    context: ctx.context.clone(),
                    context_tokens: tokens,
                    declared: true,
                    group: None,
                },
            );
            if !pushed {
                break 'walk;
            }
            continue;
        }

        let raw = eref.text().collect::<String>();
        let text = raw.trim();
        if text.is_empty() {
            continue;
        }

        // --- Pass A: whole-element fullmatch.
        let mut ctx: Option<ElementContext> = None;
        if let Some(p) = parse_path_token(text) {
            let c = ctx.get_or_insert_with(|| ElementContext::for_element(node, None));
            let stem = basename_stem(&p.path_hint);
            let tokens = c.tokens_for(p.symbol_member.as_deref(), None, Some(&stem));
            let pushed = push_ref(
                &mut out,
                &mut group,
                CodeRef {
                    ordinal: 0,
                    kind: p.kind,
                    raw_text: text.to_string(),
                    path_hint: Some(p.path_hint),
                    line_start: p.line_start,
                    line_end: p.line_end,
                    line_spans: p.line_spans,
                    symbol_container: None,
                    symbol_member: p.symbol_member,
                    context: c.context.clone(),
                    context_tokens: tokens,
                    declared: false,
                    group: None,
                },
            );
            if !pushed {
                break 'walk;
            }
            continue;
        }
        if let Some(s) = parse_symbol_token(text) {
            let c = ctx.get_or_insert_with(|| ElementContext::for_element(node, None));
            let tokens = c.tokens_for(s.member.as_deref(), last_ns_segment(&s.container), None);
            let pushed = push_ref(
                &mut out,
                &mut group,
                CodeRef {
                    ordinal: 0,
                    kind: s.kind,
                    raw_text: text.to_string(),
                    path_hint: None,
                    line_start: None,
                    line_end: None,
                    line_spans: None,
                    symbol_container: Some(s.container),
                    symbol_member: s.member,
                    context: c.context.clone(),
                    context_tokens: tokens,
                    declared: false,
                    group: None,
                },
            );
            if !pushed {
                break 'walk;
            }
            continue;
        }

        // --- Pass B: token fallback, PATH family only.
        if text.len() > TOKEN_SCAN_MAX_BYTES {
            continue;
        }
        let mut seen_in_el: std::collections::HashSet<(CodeRefKind, String)> =
            std::collections::HashSet::new();
        for (line_idx, line) in text.lines().enumerate() {
            for tok_raw in RE_TOKEN_SPLIT.split(line) {
                // Trailing punctuation only — a LEADING dot strip would
                // resurrect `.before_deploy.rb` as a false positive.
                let tok = tok_raw.trim_end_matches(['.', ',', ':', ';']);
                if tok.is_empty() {
                    continue;
                }
                let Some(p) = parse_path_token(tok) else {
                    continue;
                };
                if p.symbol_member.is_some() {
                    continue; // `path#member` is Pass-A only
                }
                if !seen_in_el.insert((p.kind, tok.to_string())) {
                    continue;
                }
                let c = ctx.get_or_insert_with(|| ElementContext::for_element(node, Some(text)));
                let stem = basename_stem(&p.path_hint);
                let tokens = c.tokens_for_line(Some(line_idx), None, None, Some(&stem));
                let pushed = push_ref(
                    &mut out,
                    &mut group,
                    CodeRef {
                        ordinal: 0,
                        kind: p.kind,
                        raw_text: tok.to_string(),
                        path_hint: Some(p.path_hint),
                        line_start: p.line_start,
                        line_end: p.line_end,
                        line_spans: p.line_spans,
                        symbol_container: None,
                        symbol_member: None,
                        context: c.context.clone(),
                        context_tokens: tokens,
                        declared: false,
                        group: None,
                    },
                );
                if !pushed {
                    break 'walk;
                }
            }
        }
    }

    out
}

/// Append one ref, materialising its group on first use. Returns `false` when
/// the doc has hit [`MAX_REFS_PER_DOC`] (caller stops the walk).
fn push_ref(out: &mut Extraction, group: &mut Option<PendingGroup>, mut r: CodeRef) -> bool {
    if out.refs.len() >= MAX_REFS_PER_DOC {
        out.truncated = true;
        return false;
    }
    match group {
        Some(g) => {
            let idx = match g.idx {
                Some(i) => i,
                None => {
                    let i = out.groups.len();
                    out.groups.push(CodeRefGroup {
                        ordinal: i as u32,
                        key: g.key.clone(),
                        label: g.label.clone(),
                        anchor: g.anchor.clone(),
                    });
                    g.idx = Some(i);
                    i
                }
            };
            r.group = Some(idx);
        }
        None => {
            r.group = None;
            out.ungrouped_count += 1;
        }
    }
    r.ordinal = out.refs.len() as u32;
    out.refs.push(r);
    true
}

/// Per-`<code>`-element context: the human string plus the machine token
/// candidates harvested from the enclosing block.
struct ElementContext {
    context: String,
    /// Sibling `<code>`/`<kbd>`/`<samp>`/`<var>` texts, document order,
    /// already filtered by [`is_context_token`].
    sibling_tokens: Vec<String>,
    /// The element's own `<pre>` text, when it has one — split into lines for
    /// the ±2-line harvest.
    pre_lines: Option<Vec<String>>,
}

impl ElementContext {
    /// `own_text` is `Some` only for a Pass-B element, whose own lines feed
    /// the `<pre>` harvest.
    fn for_element(node: ego_tree::NodeRef<'_, Node>, own_text: Option<&str>) -> Self {
        let block = nearest_block(node);
        let context = match block.and_then(ElementRef::wrap) {
            Some(b) => truncate_bytes(
                &collapse_ws(&b.text().collect::<String>()),
                CONTEXT_MAX_BYTES,
            ),
            None => String::new(),
        };
        let mut sibling_tokens = Vec::new();
        if let Some(b) = block {
            for d in b.descendants() {
                if d.id() == node.id() {
                    continue; // the ref's own element
                }
                let Some(el) = d.value().as_element() else {
                    continue;
                };
                if !CONTEXT_TOKEN_TAGS.contains(&el.name()) {
                    continue;
                }
                if has_skip_ancestor(d, TEXT_SKIP_TAGS) {
                    continue;
                }
                let Some(er) = ElementRef::wrap(d) else {
                    continue;
                };
                let t = collapse_ws(&er.text().collect::<String>());
                if is_context_token(&t) {
                    sibling_tokens.push(t);
                }
            }
        }
        // A `<pre>` ancestor means the ref sits inside a listing; the ±2-line
        // window replaces the (empty) sibling harvest there.
        let pre_lines = own_text
            .filter(|_| in_pre(node))
            .map(|t| t.lines().map(str::to_string).collect());
        ElementContext {
            context,
            sibling_tokens,
            pre_lines,
        }
    }

    fn tokens_for(
        &self,
        member: Option<&str>,
        container_tail: Option<&str>,
        path_stem: Option<&str>,
    ) -> Vec<String> {
        self.tokens_for_line(None, member, container_tail, path_stem)
    }

    fn tokens_for_line(
        &self,
        line_idx: Option<usize>,
        member: Option<&str>,
        container_tail: Option<&str>,
        path_stem: Option<&str>,
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut bytes = 0usize;
        let push = |tok: &str, out: &mut Vec<String>, bytes: &mut usize| {
            if out.len() >= CONTEXT_TOKENS_MAX || !is_context_token(tok) {
                return;
            }
            if out.iter().any(|t| t == tok) {
                return;
            }
            let add = tok.len() + usize::from(!out.is_empty());
            if *bytes + add > CONTEXT_TOKENS_MAX_BYTES {
                return;
            }
            *bytes += add;
            out.push(tok.to_string());
        };
        for t in [member, container_tail, path_stem].into_iter().flatten() {
            push(t, &mut out, &mut bytes);
        }
        for t in &self.sibling_tokens {
            push(t, &mut out, &mut bytes);
        }
        if let (Some(lines), Some(idx)) = (self.pre_lines.as_ref(), line_idx) {
            let lo = idx.saturating_sub(PRE_CONTEXT_LINE_RADIUS);
            let hi = (idx + PRE_CONTEXT_LINE_RADIUS).min(lines.len().saturating_sub(1));
            for line in &lines[lo..=hi.max(lo)] {
                for tok in RE_IDENT_SPLIT.split(line) {
                    push(tok, &mut out, &mut bytes);
                }
            }
        }
        out
    }
}

/// Nearest ancestor element whose tag is in `parser::BLOCK_TAGS`, else the
/// document's `<body>` (or root) — never `None` in practice.
fn nearest_block(node: ego_tree::NodeRef<'_, Node>) -> Option<ego_tree::NodeRef<'_, Node>> {
    let mut cur = node.parent();
    let mut last = None;
    while let Some(p) = cur {
        if let Some(el) = p.value().as_element() {
            if BLOCK_TAGS.contains(&el.name()) {
                return Some(p);
            }
            if el.name() == "body" {
                return Some(p);
            }
            last = Some(p);
        }
        cur = p.parent();
    }
    last
}

fn in_pre(node: ego_tree::NodeRef<'_, Node>) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if let Some(el) = p.value().as_element() {
            if el.name() == "pre" {
                return true;
            }
        }
        cur = p.parent();
    }
    false
}

fn basename_stem(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.split('.').next().unwrap_or(base).to_string()
}

fn last_ns_segment(container: &str) -> Option<&str> {
    container.rsplit("::").next()
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to at most `max` BYTES, never mid-UTF-8.
fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(e: &Extraction) -> Vec<(&'static str, &str)> {
        e.refs
            .iter()
            .map(|r| (r.kind.as_str(), r.raw_text.as_str()))
            .collect()
    }

    fn one(html: &str) -> Extraction {
        extract(html)
    }

    // --- extension whitelist ------------------------------------------------

    #[test]
    fn ext_whitelist_longest_suffix_wins() {
        assert_eq!(longest_code_ext("assets.js.erb"), Some("js.erb"));
        assert_eq!(longest_code_ext("_results.html.erb"), Some("html.erb"));
        assert_eq!(longest_code_ext("plain.erb"), Some("erb"));
        assert_eq!(longest_code_ext("dispatcher.rb"), Some("rb"));
    }

    #[test]
    fn doc_extensions_are_not_code_extensions() {
        for d in DOC_EXTENSIONS {
            assert!(
                !CODE_EXTENSIONS.contains(d),
                "{d} must stay out of CODE_EXTENSIONS"
            );
            assert_eq!(longest_code_ext(&format!("notes/design.{d}")), None);
        }
    }

    #[test]
    fn sql_and_env_are_not_extensions() {
        assert_eq!(longest_code_ext("Arel.sql"), None);
        assert_eq!(longest_code_ext("request.env"), None);
        assert_eq!(longest_code_ext("process.env"), None);
        assert_eq!(longest_code_ext("jest.config"), None);
    }

    // --- hard rejects -------------------------------------------------------

    #[test]
    fn hard_rejects_run_on_decoded_text() {
        // The rejects are applied to what `ElementRef::text()` yields — the
        // DECODED string, so a source `=&gt;` arrives here as `=>`.
        assert!(is_hard_rejected("a => b"));
        let e = one("<body><p><code>Hash =&gt; Array</code></p></body>");
        assert!(e.refs.is_empty(), "{:?}", kinds(&e));
        // The decode is load-bearing in the POSITIVE direction too: a numeric
        // entity for `:` yields a line ref that a raw-bytes scan (which would
        // see `&#58;`, and refuse it on the `&`) could never produce.
        let d = one("<body><p><code>app/models/order.rb&#58;77</code></p></body>");
        assert_eq!(kinds(&d), vec![("path_line", "app/models/order.rb:77")]);
    }

    // invariant:2 closed-grammar
    #[test]
    fn bang_and_question_are_not_hard_rejects() {
        assert!(parse_symbol_token("Store::Products::Indexer#indexable?").is_some());
        assert!(parse_symbol_token("Order#confirm_payment!").is_some());
    }

    #[test]
    fn leading_dot_tokens_are_rejected() {
        for t in [
            ".rb",
            ".js.erb",
            ".before_deploy.rb",
            ".to_f",
            ".env",
            ".shop-Hits-item",
        ] {
            assert!(parse_path_token(t).is_none(), "{t} must not parse");
        }
    }

    #[test]
    fn directory_refs_are_rejected() {
        for t in [
            "app/",
            "spec/",
            "app/javascript",
            "app/tasks/maintenance/search/",
            "/app/models/order.rb",
        ] {
            assert!(parse_path_token(t).is_none(), "{t} must not parse");
        }
    }

    #[test]
    fn doc_extension_refs_are_rejected_even_with_line() {
        assert!(parse_path_token("plan-a2.html:390").is_none());
        assert!(parse_path_token("notes/design.md").is_none());
        assert!(parse_path_token("README.markdown").is_none());
    }

    /// W1.gate: a bare dotted-extension-chain token with no real stem is
    /// refused — the gate measured `html.erb` ×6 as author shorthand, never
    /// a citation to a file actually named that. Every `CODE_EXTENSIONS`
    /// compound entry shares the same shape and gets the same refusal;
    /// `rb` alone was ALREADY impossible via the whitelist match (no dot to
    /// anchor the extension) — pinned here so a future refactor can't
    /// silently regress it. `foo.html.erb` (a genuine stem) must still parse.
    #[test]
    fn bare_extension_chain_tokens_are_rejected() {
        for t in [
            "html.erb",
            "js.erb",
            "json.erb",
            "css.erb",
            "turbo_stream.erb",
            "rb",
        ] {
            assert!(parse_path_token(t).is_none(), "{t} must not parse");
        }
        assert!(parse_path_token("foo.html.erb").is_some());
        assert_eq!(
            parse_path_token("foo.html.erb").unwrap().path_hint,
            "foo.html.erb"
        );
    }

    #[test]
    fn version_pinned_package_names_are_rejected() {
        for t in ["shopclient-3.12.2", "search-insights@2.17.3", "1.18.0"] {
            assert!(parse_path_token(t).is_none(), "{t} must not parse");
        }
    }

    // --- path family --------------------------------------------------------

    #[test]
    fn gem_paths_are_external() {
        let a = parse_path_token("shopclient-3.12.2/lib/shopclient/api_client.rb:120").unwrap();
        assert_eq!(a.kind, CodeRefKind::External);
        assert_eq!(a.line_start, Some(120));
        let b = parse_path_token(
            "activesupport-7.1.5.2/lib/active_support/core_ext/object/json.rb:126-138",
        )
        .unwrap();
        assert_eq!(b.kind, CodeRefKind::External);
        assert_eq!((b.line_start, b.line_end), (Some(126), Some(138)));
        let c = parse_path_token("node_modules/lodash/index.js").unwrap();
        assert_eq!(c.kind, CodeRefKind::External);
    }

    #[test]
    fn vendor_dir_is_not_external() {
        let p = parse_path_token("vendor/javascript/search-insights.js").unwrap();
        assert_eq!(p.kind, CodeRefKind::Path);
    }

    #[test]
    fn comma_line_lists_yield_one_ref_with_spans() {
        let p = parse_path_token("checkout_controller.rb:425,440").unwrap();
        assert_eq!(p.kind, CodeRefKind::PathList);
        assert_eq!(p.line_start, Some(425));
        assert_eq!(p.line_end, None);
        assert_eq!(p.line_spans.as_deref(), Some("425,440"));
        let e = one("<body><p><code>checkout_controller.rb:425,440</code></p></body>");
        assert_eq!(e.refs.len(), 1, "ONE ref carrying every span, never N");
    }

    #[test]
    fn mixed_ranges_and_lists() {
        let p = parse_path_token("assets.js.erb:30,51-65,113-119").unwrap();
        assert_eq!(p.kind, CodeRefKind::PathList);
        assert_eq!((p.line_start, p.line_end), (Some(30), None));
        assert_eq!(p.line_spans.as_deref(), Some("30,51-65,113-119"));
        let r = parse_path_token("search_service.rb:41-55").unwrap();
        assert_eq!(r.kind, CodeRefKind::PathRange);
        assert_eq!((r.line_start, r.line_end), (Some(41), Some(55)));
        assert_eq!(r.line_spans, None);
    }

    /// An overflowing line number must never be stored as a wrong hint (a
    /// prior `unwrap_or(0)` here would silently write `line_start = Some(0)`)
    /// — the whole `:LINES` suffix is dropped instead, and the path survives
    /// as an ordinary `path` ref with no line fields.
    #[test]
    fn line_number_overflow_drops_the_suffix_not_the_path() {
        let bare = parse_path_token("foo.rb:12345678901").unwrap();
        assert_eq!(bare.kind, CodeRefKind::Path);
        assert_eq!(bare.path_hint, "foo.rb");
        assert_eq!(bare.line_start, None);
        assert_eq!(bare.line_end, None);
        assert_eq!(bare.line_spans, None);

        // A valid range START next to an overflowing END must not collapse
        // onto the start (the old `unwrap_or_else(|_| a.parse()...)` did
        // exactly that, turning `5-99999999999` into `(5, Some(5))`) — it
        // drops the same way as the bare-overflow case above.
        let range_end = parse_path_token("foo.rb:5-99999999999").unwrap();
        assert_eq!(range_end.kind, CodeRefKind::Path);
        assert_eq!(range_end.path_hint, "foo.rb");
        assert_eq!(range_end.line_start, None);
        assert_eq!(range_end.line_end, None);
        assert_eq!(range_end.line_spans, None);
    }

    /// [B2] `foo.rb:0` parses as a valid `u32` — the overflow guard above
    /// never sees it — but line 0 doesn't exist, so it gets the SAME
    /// treatment as an overflowing number: drop the whole `:LINES` suffix,
    /// keep the path. Same for a zero or backwards range end.
    #[test]
    fn zero_or_backwards_line_number_drops_the_suffix_not_the_path() {
        let zero_bare = parse_path_token("foo.rb:0").unwrap();
        assert_eq!(zero_bare.kind, CodeRefKind::Path);
        assert_eq!(zero_bare.path_hint, "foo.rb");
        assert_eq!(zero_bare.line_start, None);
        assert_eq!(zero_bare.line_end, None);
        assert_eq!(zero_bare.line_spans, None);

        let zero_end = parse_path_token("foo.rb:5-0").unwrap();
        assert_eq!(zero_end.kind, CodeRefKind::Path);
        assert_eq!(zero_end.path_hint, "foo.rb");
        assert_eq!(zero_end.line_start, None);
        assert_eq!(zero_end.line_end, None);
        assert_eq!(zero_end.line_spans, None);

        let backwards = parse_path_token("foo.rb:5-3").unwrap();
        assert_eq!(backwards.kind, CodeRefKind::Path);
        assert_eq!(backwards.path_hint, "foo.rb");
        assert_eq!(backwards.line_start, None);
        assert_eq!(backwards.line_end, None);
        assert_eq!(backwards.line_spans, None);

        // A zero START inside a path_list's non-first span is a different
        // code path (list handling lives above `first_span`) — untouched by
        // this fix; `foo.rb:0` as the whole spans string is the case B2
        // covers.
    }

    #[test]
    fn path_hash_member_form() {
        let p = parse_path_token("orders_controller.rb#confirm").unwrap();
        assert_eq!(p.kind, CodeRefKind::Path);
        assert_eq!(p.path_hint, "orders_controller.rb");
        assert_eq!(p.symbol_member.as_deref(), Some("confirm"));
        // `path#member` is defined over kind=path ONLY.
        assert!(parse_path_token("orders_controller.rb:12#confirm").is_none());
    }

    // --- symbols ------------------------------------------------------------

    // invariant:2 closed-grammar
    #[test]
    fn symbol_const_requires_double_colon() {
        for t in [
            "Product",
            "EUR",
            "GET",
            "FIXME",
            "HW3T8WVS73",
            "API_WRITE_KEY",
            "BundleDescription",
        ] {
            assert!(parse_symbol_token(t).is_none(), "{t} must not be a symbol");
        }
        let s = parse_symbol_token("Billing::InvoiceError").unwrap();
        assert_eq!(s.kind, CodeRefKind::SymbolConst);
        assert_eq!(s.container, "Billing::InvoiceError");
        assert_eq!(s.member, None);
    }

    #[test]
    fn symbol_method_allows_bare_lhs() {
        let s = parse_symbol_token("Dispatcher#dispatch").unwrap();
        assert_eq!(s.kind, CodeRefKind::SymbolMethod);
        assert_eq!(s.container, "Dispatcher");
        assert_eq!(s.member.as_deref(), Some("dispatch"));
    }

    #[test]
    fn combined_namespace_class_method() {
        let s = parse_symbol_token("Checkout::UpdateCartService#item_attributes_for").unwrap();
        assert_eq!(s.container, "Checkout::UpdateCartService");
        assert_eq!(s.member.as_deref(), Some("item_attributes_for"));
    }

    // --- issues -------------------------------------------------------------

    #[test]
    fn issue_href_strips_fragment_and_query() {
        let a = parse_issue_href("https://github.com/acme/shopfront/issues/15351").unwrap();
        assert_eq!(a.number, 15351);
        let b = parse_issue_href(
            "https://github.com/acme/shopfront/issues/15351#issuecomment-5215243676",
        )
        .unwrap();
        assert_eq!(b, a);
        let c = parse_issue_href("HTTPS://GitHub.com/acme/shopfront/issues/15351/?x=1").unwrap();
        assert_eq!(c, a);
        assert!(parse_issue_href("https://github.com/acme/shopfront/pull/1").is_none());
        assert!(parse_issue_href("https://gitlab.com/a/b/issues/1").is_none());
    }

    // --- declared -----------------------------------------------------------

    #[test]
    fn declared_ref_bypasses_inference() {
        let e = one(
            r#"<body><p><code data-kb-ref="app/models/order.rb#L120-L140">the order model</code></p></body>"#,
        );
        assert_eq!(e.refs.len(), 1);
        let r = &e.refs[0];
        assert!(r.declared);
        assert_eq!(r.raw_text, "app/models/order.rb#L120-L140");
        assert_eq!(r.path_hint.as_deref(), Some("app/models/order.rb"));
        assert_eq!((r.line_start, r.line_end), (Some(120), Some(140)));
        assert_eq!(r.kind, CodeRefKind::Path);
    }

    #[test]
    fn malformed_declared_ref_is_kept_loudly() {
        let d = parse_declared_ref("app/models/nope..rb");
        assert!(!d.well_formed);
        assert_eq!(d.kind, CodeRefKind::Path);
        assert_eq!(d.path_hint, "app/models/nope..rb");
        let e = one(r#"<body><p><code data-kb-ref="app/models/nope..rb">x</code></p></body>"#);
        assert_eq!(e.refs.len(), 1, "present, not dropped");
        assert!(e.refs[0].declared);
    }

    #[test]
    fn declared_gem_path_is_still_external() {
        let d = parse_declared_ref("shopclient-3.12.2/lib/shopclient/api_client.rb");
        assert!(d.well_formed);
        assert_eq!(d.kind, CodeRefKind::External);
    }

    // --- kb-code-rev --------------------------------------------------------

    #[test]
    fn code_rev_parses_and_rejects_garbage() {
        let r = parse_code_rev("shopfront@bcd13a1d3+dirty").unwrap();
        assert_eq!(r.label, "shopfront");
        assert_eq!(r.sha, "bcd13a1d3");
        assert!(r.dirty);
        for bad in ["", "@", "x@zz", "shopfront@bcd", "shopfront bcd13a1d3"] {
            assert!(parse_code_rev(bad).is_none(), "{bad} must not parse");
        }
    }

    #[test]
    fn code_rev_round_trips() {
        for rev in [
            CodeRev {
                label: "shopfront".into(),
                sha: "bcd13a1d3".into(),
                dirty: true,
            },
            CodeRev {
                label: "shop-front.v2".into(),
                sha: "0123456789abcdef0123456789abcdef01234567".into(),
                dirty: false,
            },
        ] {
            assert_eq!(parse_code_rev(&rev.to_wire()), Some(rev));
        }
    }

    #[test]
    fn kind_wire_spelling_round_trips() {
        for k in [
            CodeRefKind::Path,
            CodeRefKind::PathLine,
            CodeRefKind::PathRange,
            CodeRefKind::PathList,
            CodeRefKind::SymbolMethod,
            CodeRefKind::SymbolConst,
            CodeRefKind::Issue,
            CodeRefKind::External,
        ] {
            assert_eq!(CodeRefKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(CodeRefKind::parse("dir"), None);
    }

    // --- extraction unit ----------------------------------------------------

    #[test]
    fn pass_b_never_emits_symbols() {
        let e = one(
            "<body><pre><code>def dispatch\n  Billing::InvoiceService.new(order).call\n  # app/models/order.rb:1044\nend</code></pre></body>",
        );
        assert!(
            e.refs
                .iter()
                .all(|r| r.kind != CodeRefKind::SymbolConst && r.kind != CodeRefKind::SymbolMethod),
            "{:?}",
            kinds(&e)
        );
        assert_eq!(kinds(&e), vec![("path_line", "app/models/order.rb:1044")]);
    }

    #[test]
    fn pass_b_skipped_over_size_cap() {
        let filler = "x ".repeat(TOKEN_SCAN_MAX_BYTES);
        let html = format!("<body><pre><code>({filler} app/models/order.rb:1)</code></pre></body>");
        let e = one(&html);
        assert!(e.refs.is_empty(), "{:?}", kinds(&e));
    }

    #[test]
    fn max_refs_per_doc_truncates_loudly() {
        let mut body = String::from("<body>");
        for i in 0..(MAX_REFS_PER_DOC + 5) {
            body.push_str(&format!("<p><code>app/models/m{i}.rb</code></p>"));
        }
        body.push_str("</body>");
        let e = one(&body);
        assert!(e.truncated);
        assert_eq!(e.refs.len(), MAX_REFS_PER_DOC);
    }

    // --- context ------------------------------------------------------------

    #[test]
    fn context_tokens_only_from_code_elements() {
        // The §6 worked example, retyped synthetically.
        let e = one(
            "<body><ul><li>Precedence inversion in <code>user_token</code>: header before the \
             <code>_SHOP</code> cookie. The only live caller is checkout \
             (<code>checkout_controller.rb:284</code> → <code>CreateOrderService</code> → \
             <code>dispatcher.rb</code>).</li></ul></body>",
        );
        let r = e
            .refs
            .iter()
            .find(|r| r.raw_text == "checkout_controller.rb:284")
            .expect("the line ref");
        assert_eq!(
            r.context_tokens,
            vec![
                "checkout_controller".to_string(),
                "user_token".into(),
                "_SHOP".into(),
                "CreateOrderService".into()
            ]
        );
        // Prose words ("Precedence", "inversion", "header", "checkout") never
        // appear.
        assert!(r.context.starts_with("Precedence inversion in user_token"));
    }

    #[test]
    fn context_truncates_on_char_boundary() {
        let prose = "à".repeat(400); // 2 bytes each
        let html = format!("<body><p>{prose} <code>app/models/order.rb</code></p></body>");
        let e = one(&html);
        let c = &e.refs[0].context;
        assert!(c.len() <= CONTEXT_MAX_BYTES);
        assert!(c.chars().all(|ch| ch == 'à'));
    }

    #[test]
    fn context_tokens_are_capped() {
        let mut li = String::from("<body><ul><li>");
        for i in 0..20 {
            li.push_str(&format!("<code>token_number_{i}</code> "));
        }
        li.push_str("<code>app/models/order.rb</code></li></ul></body>");
        let e = one(&li);
        let r = e.refs.last().unwrap();
        assert!(r.context_tokens.len() <= CONTEXT_TOKENS_MAX);
        assert!(r.context_tokens.join(" ").len() <= CONTEXT_TOKENS_MAX_BYTES);
    }

    // --- groups -------------------------------------------------------------

    #[test]
    fn group_anchor_prefers_explicit_id() {
        let e = one(
            "<body><h2 id=\"explicit-anchor\">B1 — Vendor</h2><p><code>orders/indexer.rb</code></p></body>",
        );
        assert_eq!(e.groups.len(), 1);
        assert_eq!(e.groups[0].key, "explicit-anchor");
        assert_eq!(e.groups[0].anchor, "explicit-anchor");
        assert_eq!(e.groups[0].key, e.groups[0].anchor, "key == anchor today");
    }

    #[test]
    fn group_anchor_derives_via_heading_id() {
        let e = one("<body><h2>Work items</h2><p><code>orders/indexer.rb</code></p></body>");
        assert_eq!(e.groups[0].anchor, heading_id("Work items", 0));
        assert_eq!(e.groups[0].anchor, "kb-h-work-items");
    }

    #[test]
    fn ancestor_id_is_never_used_as_anchor() {
        let e = one(
            "<body><div class=\"wi\" id=\"wi-a2\"><h3>A2 — Conversion rewrite</h3>\
             <p><code>dispatcher.rb</code></p></div></body>",
        );
        assert_eq!(e.groups[0].anchor, "kb-h-a2-conversion-rewrite");
        assert_ne!(e.groups[0].anchor, "wi-a2");
    }

    #[test]
    fn refs_before_first_heading_are_ungrouped() {
        let e = one(
            "<body><h1>Title</h1><p><code>config/importmap.rb</code> and <code>dispatcher.rb</code></p>\
             <h2>Work items</h2><p><code>orders/indexer.rb</code></p></body>",
        );
        assert_eq!(e.ungrouped_count, 2);
        assert_eq!(e.refs[0].group, None);
        assert_eq!(e.refs[1].group, None);
        assert_eq!(e.refs[2].group, Some(0));
        // R9: no sentinel entry for "ungrouped".
        assert_eq!(e.groups.len(), 1);
        assert!(e.groups.iter().all(|g| !g.key.is_empty()));
    }

    #[test]
    fn groups_list_only_ref_bearing_headings() {
        let e = one("<body><h2>Why reshuffle</h2><p>no refs here</p>\
             <h2>Work items</h2><p><code>orders/indexer.rb</code></p></body>");
        assert_eq!(e.groups.len(), 1);
        assert_eq!(e.groups[0].label, "Work items");
        assert_eq!(e.groups[0].ordinal, 0);
        // The empty heading still consumed an ordinal, so the derived slug of
        // the second one matches the runtime's.
        assert_eq!(e.groups[0].anchor, heading_id("Work items", 1));
    }

    #[test]
    fn ref_inside_heading_joins_that_heading_group() {
        let e = one("<body><h2>A1</h2><p><code>dispatcher.rb</code></p>\
             <h3>A2 · <a href=\"https://github.com/acme/shopfront/issues/15357\">#15357</a></h3>\
             <p><code>orders/indexer.rb</code></p></body>");
        let issue = e
            .refs
            .iter()
            .find(|r| r.kind == CodeRefKind::Issue)
            .unwrap();
        assert_eq!(
            e.groups[issue.group.unwrap()].label,
            "A2 · #15357",
            "group opens when the heading is ENTERED"
        );
    }

    #[test]
    fn issue_refs_dedup_on_owner_repo_number() {
        let e = one(
            "<body><p><a href=\"https://github.com/acme/shopfront/issues/15351\">a</a> \
             <a href=\"https://github.com/acme/shopfront/issues/15351#issuecomment-1\">b</a></p></body>",
        );
        let issues: Vec<_> = e
            .refs
            .iter()
            .filter(|r| r.kind == CodeRefKind::Issue)
            .collect();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path_hint.as_deref(), Some("acme/shopfront"));
        assert_eq!(issues[0].line_start, Some(15351));
        assert_eq!(
            issues[0].raw_text,
            "https://github.com/acme/shopfront/issues/15351"
        );
    }

    #[test]
    fn skip_tag_subtrees_are_never_scanned() {
        let e = one(
            "<body><template id=\"kb-prompt\"><p><code>foo/nope.rb:12</code></p></template>\
             <noscript><p><code>app/nope_noscript.rb:3</code></p></noscript>\
             <style>code { x: 1 } app/nope_style.rb:1</style>\
             <p><code>orders/indexer.rb</code></p></body>",
        );
        assert!(
            e.refs.iter().all(|r| !r.raw_text.contains("nope")),
            "{:?}",
            kinds(&e)
        );
        assert_eq!(kinds(&e), vec![("path", "orders/indexer.rb")]);
    }

    #[test]
    fn nested_markup_inside_code_reads_text_content() {
        let e = one(
            "<body><p><code>widget(<strong>clickAnalytics=true</strong>)</code> \
             <code>app/models/order.rb:<strong>77</strong></code></p></body>",
        );
        assert_eq!(kinds(&e), vec![("path_line", "app/models/order.rb:77")]);
    }

    #[test]
    fn extraction_is_deterministic() {
        let html = "<body><h2>S</h2><p><code>a/b.rb:1-2</code> <code>X::Y#z</code></p></body>";
        assert_eq!(one(html), one(html));
    }
}
