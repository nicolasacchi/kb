//! kbc-prose/1 (V76-B3, design D9-a generalised) — prose is never raw. Every
//! prose field this daemon serves (finding `title`/`rationale`/
//! `recommendation`, report `summary`/`verdict_body`, claim `body_md`,
//! timeline `body_md`, review-comment bodies, the review document's
//! `summary_md`) gains an additive `refs` array computed PER REQUEST, and
//! `POST /api/prose/resolve` (`kbc-prose-refs/1`) answers the same question
//! for client-composed prose. Nothing is persisted — root CLAUDE.md
//! invariant #2's "kb-code mints classes, nothing is cached", applied to a
//! lane whose whole job is pointing at things from prose.
//!
//! Modelled on kb-core's `coderefs` extractor (the doc→code half of the DCB
//! bridge), with the two adaptations the different input surface forces:
//!
//! * **The input is a raw Markdown SOURCE string, not parsed HTML.** coderefs
//!   consumes `EnrichCtx::html` (backtick spans arrive as real `<code>`
//!   elements); a finding's `rationale` is the SOURCE, so this module runs
//!   its own tiny structural pre-pass — fenced blocks, inline backtick spans
//!   and `[text](dest)` Markdown links are located by byte range, and the
//!   token scan skips fences and links. A ref-looking token inside a fence
//!   or a Markdown link is NOT extracted (the no-double-linking rule: a
//!   token that is already a link's text or destination must not grow a
//!   second link).
//! * **Spans are UTF-16 code-unit offsets, not bytes.** The only consumer of
//!   a span is the SPA (`web-code/src/components/prose/ProseBlock.tsx`),
//!   which slices JS strings — the same reason `search::matcher` converts
//!   nucleo's char positions to UTF-16 once, server-side (crate invariant
//!   16(a)). The golden corpus carries a non-ASCII case so a "simplification"
//!   to byte offsets fails loudly.
//!
//! # The closed grammar
//!
//! * `path` — a path whose FINAL extension is whitelisted by `syntax/1`'s
//!   `REGISTRY` (the ONE file-type declaration, crate invariant 18(a) — this
//!   module adds no second extension list), optionally suffixed `:LINE`,
//!   `:A-B`, `:A,B-C,…` or `#member`. Line numbers are HINTS, passed through
//!   verbatim; this lane NEVER guesses a line.
//! * `symbol` — `Namespace::Class` (the `::` is REQUIRED — a bare CapWord is
//!   never a symbol), `Class#method`, `Namespace::Class#method`,
//!   `Class.method`. The container/member character classes are coderefs'
//!   own.
//! * `finding` — an `f-<slug>` matching `review_findings::is_valid_finding_slug`
//!   (the ONE slug predicate — never a second copy of the pattern).
//! * `code` — an inline backtick span, surfaced so the interplay between the
//!   renderer (which already styles it) and the overlay (a ref may sit
//!   INSIDE one) is tested, not rendered twice. The SPA ignores this kind.
//! * `call` — a bare `snake_case_name(` call-looking token, extracted ONLY
//!   inside an inline backtick span. Never resolved to `exact` (a bare name
//!   with no container is exactly the ambiguity the symbol ladder's exact
//!   rung exists to refuse).
//!
//! Everything else is refused on purpose: a bare CapWord, a URL (`://` is a
//! hard reject), a directory (`trailing /`), a token with shell/regex
//! punctuation.
//!
//! # Resolution (per request, through the EXISTING ladders)
//!
//! A hint resolves to `exact | likely | candidate | orphan` — kb-code mints
//! the class; the extractor mints only hints:
//!
//! * `path` → `exact` iff `Store::get_file` has the path in the mirror index
//!   (the mirror tracks the review checkout's tip during a review), else
//!   `orphan` with a caption. The hinted line is echoed, never invented.
//! * `symbol` const → the entity index (`Store::entity_defs_for_name` +
//!   `entities::class_for`, crate invariant 13), falling back to the symbols
//!   table's exact `(name, container)` rung (the same rung
//!   `symbol_addr::resolve_symbol_at` calls `exact`) so non-Ruby constants
//!   resolve too. More than one distinct FQN caps at `candidate` and SAYS
//!   how many answered.
//! * `symbol` method → `Store::symbols_named_in_repo` filtered by container:
//!   exactly one full-container match is `exact`; a unique
//!   last-segment-container match is `likely`; several is `candidate` with
//!   the count; none is `orphan` with the caption.
//! * `finding` → this daemon's own `review_findings` row is `exact` (the
//!   `cards::trust_for` precedent); absent is `orphan`; no review in context
//!   is an `orphan` whose caption says THAT, never a silent plain-text drop.
//! * `call` → a unique same-named symbol is `likely` (NEVER `exact` —
//!   above), several is `candidate`, none carries NO resolution and renders
//!   as plain code.
//! * `code` → never resolved; `resolution` is absent.
//!
//! # The routes
//!
//! There is deliberately NO `GET /api/prose/refs`: the refs ride the wire
//! that already carries the prose. The one standalone route is
//! `POST /api/prose/resolve` — a read-shaped POST (its payload IS the
//! contract), registered on the bearer `api` router beside `/code-actions`,
//! the other read-shaped POST. It joins invariant 15's `RouteContract` walk
//! as [`V76_B3_ROUTES`]: `params_accept_without` deserialises
//! [`ProseResolveBody`] (the JSON body is the contract) so a required field
//! the CLI omits still fails by name, the same way a GET query-param route
//! does. The boards/recipe MUTATIONS stay absent from their own lists —
//! those write; this POST is a read.

use crate::entities::RouteContract;
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

/// The standalone resolve route's response schema tag.
pub const SCHEMA: &str = "kbc-prose-refs/1";

/// Hard cap on refs emitted per prose field. A field that trips it stores
/// the first N and sets [`FieldRefs::truncated`] — honest, never a silent
/// drop (the `coderefs::MAX_REFS_PER_DOC` precedent).
pub const MAX_REFS_PER_FIELD: usize = 64;

/// Body cap for `POST /api/prose/resolve`'s `text` — 64 KiB covers every
/// prose field this daemon serves with generous headroom (claims cap their
/// own `body_md` far lower); over it the route REFUSES with the number,
/// naming the fix (split the text), rather than truncating silently.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The `resolution.state` vocabulary: the three trust classes are
/// [`crate::resolve`]'s own constants; `orphan` is this lane's fourth state
/// (the hint is well-formed and points at nothing this daemon can see).
pub const STATE_ORPHAN: &str = "orphan";

// --- wire shapes -----------------------------------------------------------

/// A `[start, end)` span into the field text, in UTF-16 code units (see the
/// module doc for why not bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// One extracted (and optionally resolved) prose reference. The hint fields
/// (`path`/`line_*`/`lines`/`container`/`member`/`slug`) are set only where
/// the kind uses them; `resolution` is absent for kinds that never resolve
/// (`code`) and for a `call` with no candidate at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProseRef {
    /// `path` | `symbol` | `finding` | `code` | `call`.
    pub kind: String,
    pub span: Span,
    /// The span's text, verbatim.
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    /// The normalized comma list for a `:a,b-c,…` suffix (`"57,60-66"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<RefResolution>,
}

/// The per-request resolution of one hint. `path`/`line` are the landing
/// target when one is known; `ent` is the entity FQN a const hint resolved
/// through (the SPA links those to the entity dossier); `caption` is the
/// honest reason for any non-`exact` state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefResolution {
    /// `exact` | `likely` | `candidate` | `orphan`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

impl RefResolution {
    fn resolved(state: &str, path: Option<String>, line: Option<u32>) -> Self {
        RefResolution {
            state: state.to_string(),
            path,
            line,
            ent: None,
            caption: None,
        }
    }

    fn orphan(caption: impl Into<String>) -> Self {
        RefResolution {
            state: STATE_ORPHAN.to_string(),
            path: None,
            line: None,
            ent: None,
            caption: Some(caption.into()),
        }
    }
}

/// One prose field's refs, exactly as served beside the field on the wire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FieldRefs {
    pub refs: Vec<ProseRef>,
    /// Hit [`MAX_REFS_PER_FIELD`].
    pub truncated: bool,
}

// --- the pure extractor ------------------------------------------------------

/// The context one [`extract`] call needs about WHERE in the Markdown
/// structure each byte sits: fence ranges and link ranges are skipped by the
/// token scan; inline-code ranges additionally enable `call` extraction.
struct Structure {
    /// Fenced code blocks (``` ``` ```/`~~~`), byte ranges, sorted.
    fences: Vec<(usize, usize)>,
    /// Inline backtick spans, byte ranges INCLUDING the backticks, sorted.
    code: Vec<(usize, usize)>,
    /// `[text](dest)` Markdown links, byte ranges, sorted.
    links: Vec<(usize, usize)>,
}

fn in_ranges(ranges: &[(usize, usize)], pos: usize) -> bool {
    ranges.iter().any(|(s, e)| pos >= *s && pos < *e)
}

/// The structural pre-pass. Total, deterministic, hand-rolled (this crate
/// carries no regex dependency outside the search lane).
fn structure_of(text: &str) -> Structure {
    let bytes = text.as_bytes();
    // 1. Fences: a line whose trimmed-start begins with 3+ backticks or
    //    tildes opens one; a line whose trimmed-start is a run of the SAME
    //    marker char at least as long closes it (CommonMark's rule, the one
    //    `markdownLite`'s fence arm and `review_doc::refs`'s scanner apply).
    let mut fences: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<(u8, usize, usize)> = None; // (marker, len, start offset)
    let mut line_start = 0usize;
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        let trimmed = body.trim_start();
        let indent = body.len() - trimmed.len();
        let tb = trimmed.as_bytes();
        if !tb.is_empty() && (tb[0] == b'`' || tb[0] == b'~') {
            let run = tb.iter().take_while(|&&b| b == tb[0]).count();
            if run >= 3 {
                match open {
                    Some((ch, len, start)) if ch == tb[0] && run >= len => {
                        fences.push((start, line_start + line.len()));
                        open = None;
                    }
                    None => open = Some((tb[0], run, line_start)),
                    _ => {}
                }
            }
        }
        line_start += line.len();
        let _ = indent;
    }
    if let Some((_, _, start)) = open {
        // An unclosed fence runs to EOF — `markdownLite` emits it rather
        // than dropping the tail; the extractor must not ref-link inside it
        // either.
        fences.push((start, text.len()));
    }

    // 2. Inline single-backtick spans outside fences (markdownLite's
    //    `parseInline` handles exactly single backticks — consistency with
    //    the renderer, not a CommonMark claim).
    let mut code: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if in_ranges(&fences, i) {
            i += 1;
            continue;
        }
        if bytes[i] == b'`' {
            if let Some(rel) = text[i + 1..].find('`') {
                let close = i + 1 + rel;
                if close > i + 1 {
                    code.push((i, close + 1));
                    i = close + 1;
                    continue;
                }
            }
        }
        i += 1;
    }

    // 3. `[text](dest)` links outside fences: `[`, up to the next `]`,
    //    immediately `(`, up to the next `)`. Anything that does not
    //    complete the shape is plain text and falls through to the token
    //    scan as usual.
    let mut links: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if in_ranges(&fences, i) {
            i += 1;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some(rel_close) = text[i + 1..].find("](") {
                let close = i + 1 + rel_close;
                if let Some(rel_paren) = text[close + 2..].find(')') {
                    let end = close + 2 + rel_paren + 1;
                    links.push((i, end));
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }

    Structure {
        fences,
        code,
        links,
    }
}

/// True when `ext` (the final dotted suffix, without the dot) is whitelisted
/// by `syntax/1`'s REGISTRY — the ONE extension declaration (crate invariant
/// 18(a)); this lane adds no second list.
fn is_code_ext(ext: &str) -> bool {
    crate::syntax::REGISTRY
        .iter()
        .any(|r| r.extensions.contains(&ext))
}

/// coderefs' hard-reject characters, hand-rolled. `$ @ { } [ ] ( ) " ' \`
/// < > * | \ % & ^ ~ ;` plus the two substrings `..` (ranges/traversal) and
/// `://` (URLs). `!`/`?` are NOT rejected (legal Ruby method suffixes);
/// `#`, `:` and `,` are load-bearing (`Class#method`, `:line`, `:a,b`).
fn has_hard_reject(tok: &str) -> bool {
    tok.contains("..")
        || tok.contains("://")
        || tok.chars().any(|c| {
            matches!(
                c,
                '$' | '@'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '"'
                    | '\''
                    | '`'
                    | '<'
                    | '>'
                    | '*'
                    | '|'
                    | '\\'
                    | '%'
                    | '&'
                    | '^'
                    | '~'
                    | ';'
            )
        })
}

/// `[a-z_][A-Za-z0-9_]*[!?]?` — coderefs' member grammar.
fn is_member(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c == '_' => {}
        _ => return false,
    }
    let stem = s.trim_end_matches(['!', '?']);
    !stem.is_empty()
        && stem.len() >= s.len() - 1
        && stem.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `[A-Z][A-Za-z0-9_]*` — one constant segment.
fn is_const_seg(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `ConstSeg (:: ConstSeg)*`.
fn is_container(s: &str) -> bool {
    !s.is_empty() && s.split("::").all(is_const_seg)
}

/// The `:LINES` grammar `\d+(-\d+)?(,\d+(-\d+)?)*`.
fn is_lines_suffix(s: &str) -> bool {
    !s.is_empty()
        && s.split(',').all(|part| {
            let mut it = part.split('-');
            let a = it.next().unwrap_or("");
            let ok_a = !a.is_empty() && a.chars().all(|c| c.is_ascii_digit());
            match it.next() {
                None => ok_a,
                Some(b) => {
                    ok_a && !b.is_empty()
                        && b.chars().all(|c| c.is_ascii_digit())
                        && it.next().is_none()
                }
            }
        })
}

/// First `(start, end)` of a `:LINES` list; `end` `None` for a bare number.
/// `None` on ANY bad number (u32 overflow) or a zero/backwards span —
/// coderefs' own rule: drop the whole suffix, keep the path, never store an
/// invented hint.
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

/// One path segment: `[A-Za-z0-9_][A-Za-z0-9_.+-]*`.
fn is_segment(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'))
}

struct PathHit {
    path: String,
    line_start: Option<u32>,
    line_end: Option<u32>,
    lines: Option<String>,
    member: Option<String>,
}

/// The path production: whitelisted-extension path, optional `:LINES`,
/// optional `#member` (on the line-less form only — coderefs' rule).
fn parse_path_token(tok: &str) -> Option<PathHit> {
    if tok.is_empty() || has_hard_reject(tok) {
        return None;
    }
    // `path#member` — split first: `#` is never legal inside a segment.
    if let Some((left, right)) = tok.split_once('#') {
        if right.contains('#') || !is_member(right) {
            return None;
        }
        let base = parse_pure_path(left)?;
        if base.line_start.is_some() {
            return None;
        }
        return Some(PathHit {
            member: Some(right.to_string()),
            ..base
        });
    }
    parse_pure_path(tok)
}

fn parse_pure_path(tok: &str) -> Option<PathHit> {
    if tok.is_empty() || has_hard_reject(tok) {
        return None;
    }
    let (body, lines) = match tok.rsplit_once(':') {
        Some((head, tail)) if !head.is_empty() && is_lines_suffix(tail) => (head, Some(tail)),
        _ => (tok, None),
    };
    // Directory refs are rejected: a repo dir has no landing surface in the
    // per-FILE reader, so it would render as doc-rot (coderefs' rule).
    if body.starts_with('/') || body.ends_with('/') {
        return None;
    }
    let ext = body.rsplit_once('.').map(|(_, e)| e)?;
    if !is_code_ext(ext) {
        return None;
    }
    let segs: Vec<&str> = body.split('/').collect();
    if segs.iter().any(|s| s.is_empty() || !is_segment(s)) {
        return None;
    }
    let base = segs[segs.len() - 1];
    // A non-empty stem: `foo.rb` yes, `.rb`/the bare extension chain no.
    if base.len() <= ext.len() + 1 {
        return None;
    }
    let (line_start, line_end, line_spans) = match lines {
        None => (None, None, None),
        Some(spans) => match first_span(spans) {
            Some((start, end)) => {
                let list = if spans.contains(',') {
                    Some(spans.to_string())
                } else {
                    None
                };
                (Some(start), end, list)
            }
            None => (None, None, None),
        },
    };
    Some(PathHit {
        path: body.to_string(),
        line_start,
        line_end,
        lines: line_spans,
        member: None,
    })
}

struct SymbolHit {
    container: String,
    member: Option<String>,
}

/// The symbol productions. `::` is MANDATORY for a bare const; a
/// `Class#method`/`Class.method` left-hand side may be a bare CapWord.
fn parse_symbol_token(tok: &str) -> Option<SymbolHit> {
    if tok.is_empty() || has_hard_reject(tok) {
        return None;
    }
    if let Some((lhs, member)) = tok.split_once('#') {
        if is_container(lhs) && is_member(member) {
            return Some(SymbolHit {
                container: lhs.to_string(),
                member: Some(member.to_string()),
            });
        }
        return None;
    }
    // `Class.method` — but never a dotted-path shape: the LHS of the LAST
    // dot must be a container and the RHS a member. (Paths were tried
    // first, so `foo.rb` never reaches here.)
    if let Some((lhs, member)) = tok.rsplit_once('.') {
        if !lhs.contains('/') && is_container(lhs) && is_member(member) {
            return Some(SymbolHit {
                container: lhs.to_string(),
                member: Some(member.to_string()),
            });
        }
    }
    if tok.contains("::") && is_container(tok) {
        return Some(SymbolHit {
            container: tok.to_string(),
            member: None,
        });
    }
    None
}

/// `[a-z_][a-z0-9_]*` immediately followed by `(` — the `call` production,
/// scanned INSIDE an inline-code span's inner text. Returns
/// `(name, byte_start_in_inner, byte_end)` per hit. Never matches
/// mid-identifier (the byte before must not be identifier-continuation).
fn scan_calls(inner: &str) -> Vec<(String, usize, usize)> {
    let bytes = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_ascii_lowercase() || c == '_' {
            let start = i;
            while i < bytes.len() {
                let d = bytes[i] as char;
                if d.is_ascii_lowercase() || d.is_ascii_digit() || d == '_' {
                    i += 1;
                } else {
                    break;
                }
            }
            if i < bytes.len() && bytes[i] == b'(' && i > start {
                let prev_ok = start == 0 || {
                    let p = bytes[start - 1] as char;
                    !(p.is_ascii_alphanumeric() || p == '_')
                };
                if prev_ok {
                    out.push((inner[start..i].to_string(), start, i));
                }
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Byte offset → UTF-16 code-unit offset, for the span the SPA slices on.
fn utf16_pos(text: &str, byte_pos: usize) -> u32 {
    text[..byte_pos].encode_utf16().count() as u32
}

/// Extract every hint from one prose field. Pure, total, deterministic —
/// no I/O, no clock, no store. Capped at [`MAX_REFS_PER_FIELD`] with an
/// honest `truncated`.
pub fn extract(text: &str) -> FieldRefs {
    let st = structure_of(text);
    let mut out = FieldRefs::default();
    let push = |out: &mut FieldRefs, r: ProseRef| -> bool {
        if out.refs.len() >= MAX_REFS_PER_FIELD {
            out.truncated = true;
            return false;
        }
        out.refs.push(r);
        true
    };

    // The `code` hints: one per inline-code span, span INCLUDING the
    // backticks, `text` the inner content.
    for (s, e) in &st.code {
        let r = ProseRef {
            kind: "code".to_string(),
            span: Span {
                start: utf16_pos(text, *s),
                end: utf16_pos(text, *e),
            },
            text: text[*s + 1..*e - 1].to_string(),
            path: None,
            line_start: None,
            line_end: None,
            lines: None,
            container: None,
            member: None,
            slug: None,
            resolution: None,
        };
        if !push(&mut out, r) {
            return out;
        }
    }

    // The token scan. Split delimiters: whitespace plus the bracket/quote
    // family; edge trim peels sentence punctuation and emphasis markers.
    let mut claimed: Vec<(usize, usize)> = Vec::new();
    let bytes = text.as_bytes();
    let is_delim = |b: u8| -> bool {
        (b as char).is_whitespace()
            || matches!(
                b,
                b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'"' | b'\'' | b'`' | b';'
            )
    };
    let mut i = 0usize;
    while i < bytes.len() {
        if is_delim(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && !is_delim(bytes[i]) {
            i += 1;
        }
        let raw_end = i;
        // Edge trim: leading emphasis/angle, trailing sentence punctuation.
        let mut s = start;
        while s < raw_end && matches!(bytes[s], b'<' | b'*' | b'~') {
            s += 1;
        }
        let mut e = raw_end;
        while e > s && matches!(bytes[e - 1], b'.' | b',' | b':' | b'*' | b'~' | b'>') {
            e -= 1;
        }
        if e <= s {
            continue;
        }
        // ASCII-fast-path guard: the delimiters and trim sets are all ASCII,
        // so s/e are always char boundaries even in non-ASCII prose.
        let tok = &text[s..e];
        if in_ranges(&st.fences, s) || in_ranges(&st.links, s) {
            continue;
        }
        let mut hint: Option<ProseRef> = None;
        if let Some(p) = parse_path_token(tok) {
            hint = Some(ProseRef {
                kind: "path".to_string(),
                span: Span {
                    start: utf16_pos(text, s),
                    end: utf16_pos(text, e),
                },
                text: tok.to_string(),
                path: Some(p.path),
                line_start: p.line_start,
                line_end: p.line_end,
                lines: p.lines,
                container: None,
                member: p.member,
                slug: None,
                resolution: None,
            });
        } else if let Some(sym) = parse_symbol_token(tok) {
            hint = Some(ProseRef {
                kind: "symbol".to_string(),
                span: Span {
                    start: utf16_pos(text, s),
                    end: utf16_pos(text, e),
                },
                text: tok.to_string(),
                path: None,
                line_start: None,
                line_end: None,
                lines: None,
                container: Some(sym.container),
                member: sym.member,
                slug: None,
                resolution: None,
            });
        } else if crate::review_findings::is_valid_finding_slug(tok) {
            hint = Some(ProseRef {
                kind: "finding".to_string(),
                span: Span {
                    start: utf16_pos(text, s),
                    end: utf16_pos(text, e),
                },
                text: tok.to_string(),
                path: None,
                line_start: None,
                line_end: None,
                lines: None,
                container: None,
                member: None,
                slug: Some(tok.to_string()),
                resolution: None,
            });
        }
        if let Some(r) = hint {
            claimed.push((s, e));
            if !push(&mut out, r) {
                return out;
            }
        }
    }

    // The `call` pass: inline-code spans only, never over a token the path/
    // symbol/finding scan already claimed.
    for (s, e) in &st.code {
        let inner = &text[*s + 1..*e - 1];
        for (name, is, ie) in scan_calls(inner) {
            let abs_s = *s + 1 + is;
            let abs_e = *s + 1 + ie;
            if claimed.iter().any(|(cs, ce)| abs_s < *ce && abs_e > *cs) {
                continue;
            }
            let r = ProseRef {
                kind: "call".to_string(),
                span: Span {
                    start: utf16_pos(text, abs_s),
                    end: utf16_pos(text, abs_e),
                },
                text: name,
                path: None,
                line_start: None,
                line_end: None,
                lines: None,
                container: None,
                member: None,
                slug: None,
                resolution: None,
            };
            if !push(&mut out, r) {
                return out;
            }
        }
    }

    // The wire is ordered by where the ref APPEARS (the `code` hints were
    // collected first for convenience, not because they sort first).
    out.refs.sort_by_key(|r| (r.span.start, r.span.end));
    out
}

// --- per-request resolution ---------------------------------------------------

/// What a resolution run needs: the repo's store id (paths, symbols,
/// entities) and — for `finding` hints — the review. `review_id: None` is a
/// real state (generic client-composed prose), not a defect.
#[derive(Debug, Clone, Copy)]
pub struct RefCtx {
    pub repo_id: i64,
    pub review_id: Option<i64>,
}

/// Extract + resolve one prose field. Store reads only; no git, no clock.
pub fn field_refs(store: &Store, ctx: &RefCtx, text: &str) -> Result<FieldRefs, ApiError> {
    let mut out = extract(text);
    resolve_refs(store, ctx, &mut out.refs)?;
    Ok(out)
}

/// Resolve each hint in place through the existing ladders (see the module
/// doc for the per-kind rules). Infallible except for real store I/O errors.
pub fn resolve_refs(store: &Store, ctx: &RefCtx, refs: &mut [ProseRef]) -> Result<(), ApiError> {
    for r in refs.iter_mut() {
        r.resolution = match r.kind.as_str() {
            "path" => Some(resolve_path(store, ctx, r)?),
            "symbol" => Some(resolve_symbol(store, ctx, r)?),
            "finding" => Some(resolve_finding(store, ctx, r)?),
            "call" => resolve_call(store, ctx, r)?,
            _ => None, // `code` — the renderer's own surface, never resolved
        };
    }
    Ok(())
}

fn resolve_path(store: &Store, ctx: &RefCtx, r: &ProseRef) -> Result<RefResolution, ApiError> {
    let path = r.path.clone().unwrap_or_default();
    match store.get_file(ctx.repo_id, &path)? {
        Some(_) => Ok(RefResolution::resolved(
            crate::resolve::CLASS_EXACT,
            Some(path),
            r.line_start,
        )),
        None => Ok(RefResolution::orphan(format!(
            "no file {path:?} in the mirror index — the path may be renamed, deleted, or never indexed"
        ))),
    }
}

fn resolve_finding(store: &Store, ctx: &RefCtx, r: &ProseRef) -> Result<RefResolution, ApiError> {
    let slug = r.slug.clone().unwrap_or_default();
    match ctx.review_id {
        Some(review_id) => match store.get_review_finding(review_id, &slug)? {
            // This daemon's own store row: the `cards::trust_for` precedent
            // mints `exact` for exactly this shape.
            Some(_) => Ok(RefResolution::resolved(
                crate::resolve::CLASS_EXACT,
                None,
                None,
            )),
            None => Ok(RefResolution::orphan(format!(
                "review {review_id} has no finding {slug}"
            ))),
        },
        None => Ok(RefResolution::orphan(format!(
            "no review in this context to resolve {slug} against"
        ))),
    }
}

fn resolve_call(
    store: &Store,
    ctx: &RefCtx,
    r: &ProseRef,
) -> Result<Option<RefResolution>, ApiError> {
    let rows = store.symbols_named_in_repo(ctx.repo_id, &r.text)?;
    // NEVER `exact` (the module doc): a bare name carries no container, so a
    // unique hit is `likely` at best.
    match rows.len() {
        0 => Ok(None),
        1 => {
            let (path, sym) = &rows[0];
            Ok(Some(RefResolution::resolved(
                crate::resolve::CLASS_LIKELY,
                Some(path.clone()),
                Some(sym.line_start),
            )))
        }
        n => Ok(Some(RefResolution {
            state: crate::resolve::CLASS_CANDIDATE.to_string(),
            path: None,
            line: None,
            ent: None,
            caption: Some(format!(
                "{n} symbols named {:?} in this repo — a bare call name cannot pick one",
                r.text
            )),
        })),
    }
}

fn resolve_symbol(store: &Store, ctx: &RefCtx, r: &ProseRef) -> Result<RefResolution, ApiError> {
    let container = r.container.clone().unwrap_or_default();
    match &r.member {
        Some(member) => resolve_method(store, ctx, &container, member),
        None => resolve_const(store, ctx, &container),
    }
}

fn resolve_method(
    store: &Store,
    ctx: &RefCtx,
    container: &str,
    member: &str,
) -> Result<RefResolution, ApiError> {
    let rows = store.symbols_named_in_repo(ctx.repo_id, member)?;
    let full: Vec<&(String, crate::extract::Symbol)> = rows
        .iter()
        .filter(|(_, s)| s.container.as_deref() == Some(container))
        .collect();
    if full.len() == 1 {
        let (path, sym) = full[0];
        return Ok(RefResolution::resolved(
            crate::resolve::CLASS_EXACT,
            Some(path.clone()),
            Some(sym.line_start),
        ));
    }
    if full.len() > 1 {
        return Ok(RefResolution {
            state: crate::resolve::CLASS_CANDIDATE.to_string(),
            path: None,
            line: None,
            ent: None,
            caption: Some(format!(
                "{} symbols named {member:?} with container {container:?} — ambiguous",
                full.len()
            )),
        });
    }
    let last = crate::entities::last_segment(container);
    let tail: Vec<&(String, crate::extract::Symbol)> = rows
        .iter()
        .filter(|(_, s)| {
            s.container
                .as_deref()
                .is_some_and(|c| crate::entities::last_segment(c) == last)
        })
        .collect();
    if tail.len() == 1 {
        let (path, sym) = tail[0];
        return Ok(RefResolution {
            state: crate::resolve::CLASS_LIKELY.to_string(),
            path: Some(path.clone()),
            line: Some(sym.line_start),
            ent: None,
            caption: Some(format!(
                "matched on the container's last segment {last:?}, not the full {container:?}"
            )),
        });
    }
    if tail.len() > 1 {
        return Ok(RefResolution {
            state: crate::resolve::CLASS_CANDIDATE.to_string(),
            path: None,
            line: None,
            ent: None,
            caption: Some(format!(
                "{} symbols named {member:?} under a *::{last} container — ambiguous",
                tail.len()
            )),
        });
    }
    Ok(RefResolution::orphan(format!(
        "no symbol {member:?} with container {container:?} in the mirror index"
    )))
}

fn resolve_const(store: &Store, ctx: &RefCtx, container: &str) -> Result<RefResolution, ApiError> {
    // The entity index first (Ruby class/module definition sites, crate
    // invariant 13): rows are CLAIMS, `entities::class_for` mints the class.
    let rows = store.entity_defs_for_name(
        ctx.repo_id,
        None,
        container,
        crate::entities::MAX_DEFS_PER_QUERY,
    )?;
    if !rows.is_empty() {
        let distinct_fqns: std::collections::BTreeSet<&str> =
            rows.iter().map(|r| r.fqn.as_str()).collect();
        let mut best: Option<(&crate::store::EntityDefRow, &'static str)> = None;
        for row in &rows {
            let matched_via = if row.fqn == container
                || row.zeitwerk_fqn.as_deref() == Some(container)
                || crate::entities::last_segment(&row.fqn)
                    == crate::entities::last_segment(container)
            {
                crate::entities::MATCHED_VIA_NESTING
            } else {
                crate::entities::MATCHED_VIA_ZEITWERK
            };
            let stale = row.live_blob_hash.as_deref() != Some(row.blob_hash.as_str());
            let class =
                crate::entities::class_for(matched_via, &row.nesting, &row.zeitwerk_state, stale);
            let better = match best {
                None => true,
                Some((_, c)) => class_rank(class) > class_rank(c),
            };
            if better {
                best = Some((row, class));
            }
        }
        let (row, class) = best.expect("rows is non-empty");
        let (class, caption) = if distinct_fqns.len() > 1 {
            (
                crate::resolve::CLASS_CANDIDATE,
                Some(format!(
                    "{} distinct constants answer to {container:?} — nothing is merged on a bare name",
                    distinct_fqns.len()
                )),
            )
        } else {
            (class, None)
        };
        return Ok(RefResolution {
            state: class.to_string(),
            path: Some(row.path.clone()),
            line: Some(row.line_start as u32),
            ent: Some(row.fqn.clone()),
            caption,
        });
    }
    // Symbols-table fallback (non-Ruby constants — the entity index covers
    // Ruby only): the exact `(name, container)` rung, the same shape
    // `symbol_addr::resolve_symbol_at` calls `exact`.
    let name = crate::entities::last_segment(container);
    let prefix = container
        .strip_suffix(name)
        .and_then(|p| p.strip_suffix("::"));
    let rows = store.symbols_named_in_repo(ctx.repo_id, name)?;
    let exact: Vec<&(String, crate::extract::Symbol)> = rows
        .iter()
        .filter(|(_, s)| s.container.as_deref() == prefix)
        .collect();
    if exact.len() == 1 {
        let (path, sym) = exact[0];
        return Ok(RefResolution::resolved(
            crate::resolve::CLASS_EXACT,
            Some(path.clone()),
            Some(sym.line_start),
        ));
    }
    if exact.len() > 1 {
        return Ok(RefResolution {
            state: crate::resolve::CLASS_CANDIDATE.to_string(),
            path: None,
            line: None,
            ent: None,
            caption: Some(format!(
                "{} symbols named {name:?} with container {prefix:?} — ambiguous",
                exact.len()
            )),
        });
    }
    Ok(RefResolution::orphan(format!(
        "no indexed class or module answers to {container:?} — the entity index covers \
         Ruby class/module definition sites; other languages resolve through the symbols table"
    )))
}

fn class_rank(class: &str) -> u8 {
    match class {
        c if c == crate::resolve::CLASS_EXACT => 3,
        c if c == crate::resolve::CLASS_LIKELY => 2,
        c if c == crate::resolve::CLASS_CANDIDATE => 1,
        _ => 0,
    }
}

// --- POST /api/prose/resolve --------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ProseResolveBody {
    pub repo: String,
    /// A review id — gives `finding` hints their slug space.
    #[serde(default)]
    pub review: Option<i64>,
    /// A patchset number or `"latest"`. Validated to EXIST when given
    /// (a `ps` naming nothing is a 404, never silently ignored — a param
    /// nobody reads is the v7.0 dead-surface defect). Resolution itself is
    /// against the current mirror; see the module doc.
    #[serde(default)]
    pub ps: Option<String>,
    pub text: String,
}

/// `POST /api/prose/resolve` — `kbc-prose-refs/1`. A read-shaped POST on the
/// bearer `api` router (the `/code-actions` precedent): computes the refs
/// for one client-composed prose string. Nothing persisted, no git, one
/// blocking-pool trip.
pub async fn prose_resolve_route(
    State(state): State<SharedState>,
    Json(body): Json<ProseResolveBody>,
) -> Result<impl IntoResponse, ApiError> {
    if body.text.len() > MAX_TEXT_BYTES {
        return Err(ApiError::bad_request(format!(
            "text is {} bytes, over the {MAX_TEXT_BYTES}-byte cap — split the prose and resolve it in pieces",
            body.text.len()
        )));
    }
    let (_repo, repo_id) = crate::routes::find_repo(&state, &body.repo)?;
    let review_id = body.review;
    if let Some(id) = review_id {
        let (review, _repo_entry, review_repo_id) =
            crate::reviews::require_review(&state, id).await?;
        if review_repo_id != repo_id {
            return Err(ApiError::bad_request(format!(
                "review {id} belongs to repo {:?}, not {:?}",
                review.repo, body.repo
            )));
        }
    }
    let ps_param = body.ps.clone();
    let text = body.text.clone();
    let out = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            // `ps` is validated, not ignored: naming a patchset the review
            // does not have is a 404 with the number.
            let ps_number = match (review_id, ps_param.as_deref()) {
                (Some(id), Some(ps)) => {
                    Some(crate::reviews::resolve_ps(store, id, Some(ps))?.ps_number)
                }
                _ => None,
            };
            let ctx = RefCtx { repo_id, review_id };
            let fr = field_refs(store, &ctx, &text)?;
            Ok((ps_number, fr))
        })
        .await?;
    let (ps_number, fr) = out;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "repo": body.repo,
            "review": review_id,
            "ps": ps_number,
            "refs": fr.refs,
            "truncated": fr.truncated,
        })),
    ))
}

/// Build the JSON object of per-field refs for one finding's three prose
/// fields, ready to merge into the finding's wire object. ONE helper for
/// both call sites (`list_findings_route`'s batch pass and
/// `compose_finding_view`) so the two can never disagree about the key
/// names.
pub(crate) fn finding_field_refs(
    store: &Store,
    ctx: &RefCtx,
    title: &str,
    rationale: &str,
    recommendation: Option<&str>,
) -> Result<serde_json::Value, ApiError> {
    let mut map = serde_json::Map::new();
    map.insert(
        "title_refs".to_string(),
        serde_json::to_value(field_refs(store, ctx, title)?)
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    );
    map.insert(
        "rationale_refs".to_string(),
        serde_json::to_value(field_refs(store, ctx, rationale)?)
            .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    );
    if let Some(rec) = recommendation {
        map.insert(
            "recommendation_refs".to_string(),
            serde_json::to_value(field_refs(store, ctx, rec)?)
                .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        );
    }
    Ok(serde_json::Value::Object(map))
}

// --- RouteContract (invariant 15) ------------------------------------------

fn accepts_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("repo", "r"), ("text", "hello")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<ProseResolveBody>(serde_json::Value::Object(map)).is_ok()
}

/// `POST /api/prose/resolve` — the JSON body is the contract (`repo` +
/// `text` required; `review`/`ps` optional). Walked from both sides
/// (this crate's dead-surface test + the CLI's
/// `cli_requests_send_every_param_their_route_requires`).
pub const PROSE_RESOLVE_ROUTE: RouteContract = RouteContract {
    path: "/api/prose/resolve",
    handler: "prose_refs::prose_resolve_route",
    required_params: &["repo", "text"],
    params_accept_without: accepts_without,
};

/// This unit's declared route surface — invariant 15's list, living beside
/// the route rather than in a test file so it is added in the SAME edit
/// that adds the route.
pub const V76_B3_ROUTES: &[RouteContract] = &[PROSE_RESOLVE_ROUTE];

#[cfg(test)]
mod tests;
