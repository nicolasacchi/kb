//! Server-side syntax highlighting: tree-sitter's official bundled
//! `highlights.scm` (same sourcing rule as `extract.rs`'s `tags.scm` — the
//! grammar crate's own const, never a filesystem path) → a non-overlapping,
//! in-bounds list of `Span { byte_start, byte_len, class }`. This is DATA
//! for a future SPA renderer to turn into `<span class="...">` — no HTML
//! is produced here, and kb-code does not depend on the `tree-sitter-
//! highlight` crate (this is a deliberately smaller, self-contained
//! algorithm — see "Overlap resolution" below for what it does NOT do).
//!
//! ## Capture-name → class mapping
//!
//! `highlights.scm` capture names are dotted scopes (`function.method`,
//! `punctuation.bracket`, `variable.parameter`, ...). `map_class` buckets
//! them into the fixed [`HighlightClass`] set, and the lookup is TWO
//! levels deep (V72-H2b): the first two scope words joined by a `.` are
//! tried first, then the top-level word alone. That is the whole widening
//! mechanism — a sub-scope earns a class only by being listed, and
//! everything else keeps falling back to exactly the bucket it had before.
//! `comment.documentation` still reads as `Comment`, `string.escape` still
//! as `String`.
//!
//! Observed top-level scopes across the bundled `highlights.scm` files:
//! `attribute`, `boolean`, `comment`, `constant`, `constructor`,
//! `embedded`, `escape`, `function`, `keyword`, `label`, `number`,
//! `operator`, `property`, `punctuation`, `string`, `tag`, `text`, `type`,
//! `variable`, plus SCSS's `spell` and Markdown's `none`. `constructor`
//! maps to `Function` (a constructor call reads like a function call);
//! CSS's `tag` (`div`, `a`, `nesting_selector`, `universal_selector`) maps
//! to `Type`, the same bucket HAML's scanner already paints a tag name
//! into; Markdown's `text`/`none` and SCSS's `spell` stay `Other`
//! deliberately — folding a heading, a URI and a spell-check marker into a
//! CODE class would be a worse lie than an honest "unclassified".
//! `embedded` (an injection-content marker, not a real highlight) is
//! `Other` for the same reason. Anything unmapped falls into `Other`
//! rather than being silently dropped.
//!
//! ## The role table (V72-H2b, D16)
//!
//! The class set IS the `kbc-theme/1` contract — every member is bound to
//! a `--syn-*` CSS variable in `web-code/src/styles/tokens.css`, derived
//! per theme by `web-code/src/themes/derive.ts`, and rendered as
//! `.kbc-hl-<role>` (`web-code/src/lib/decorations.ts`). D16 widens it
//! from fifteen to EIGHTEEN, and the three new members were picked by
//! counting the capture names the queries in this build actually emit —
//! not by wish. Coverage, over the fourteen `highlights.scm` sources
//! `lang::highlights_query` returns (TypeScript and TSX both concatenate
//! JavaScript's):
//!
//! | new class            | capture names                                                              | grammars |
//! |----------------------|----------------------------------------------------------------------------|----------|
//! | `ConstantBuiltin`    | `constant.builtin`, plus YAML/TOML's top-level `boolean`                    | 9        |
//! | `PunctuationSpecial` | `punctuation.special`                                                       | 7        |
//! | `StringSpecial`      | `string.special`, `string.special.regex`, `.symbol`, `.key`                 | 7        |
//!
//! `boolean` moves with them: `true`/`false`/`~`/`null` ARE builtin
//! constants, and painting them as user constants was the closest honest
//! bucket only while `ConstantBuiltin` did not exist.
//!
//! MEASURED AND NOT ADOPTED, so the choice is on record rather than an
//! oversight (the `usages2::UNMINTED_KINDS` precedent, pinned by
//! `deferred_role_candidates_are_recorded_with_their_evidence`):
//! `constructor` (6 grammars), `variable.parameter` (5 — JavaScript emits
//! it only from `highlights-params.scm`, which `HIGHLIGHT_QUERY` does not
//! include), `type.builtin` (3), `namespace` (**0** — no grammar in this
//! build emits it AT ALL; CSS has an `@namespace` AT-RULE, which is a
//! keyword in the query's pattern text and not a capture name, and reading
//! one for the other is exactly the mistake a COUNTED table exists to
//! catch), and Markdown's `text.*` family
//! (1 grammar, 4 captures — one bucket for headings, links, literals and
//! references would be a fold, and four would spend every remaining slot
//! on one file type). D16's budget is "~18"; these are what 21 would have
//! been.
//!
//! Widening this set bumps [`ROLE_TABLE_VERSION`], which every language's
//! `lang::LangInfo::highlight_salt` embeds — so the whole corpus re-paints
//! exactly once and no symbol row is touched. Price it first:
//! `kb-code reextract --bill`.
//!
//! **An `Other` never OVERWRITES a real class for the same exact range**
//! (V72-H2a). SCSS's query captures its `//` comments twice
//! (`(js_comment) @comment @spell`), and under a plain last-write-wins
//! dedup the unmapped `@spell` would have won and every SCSS line comment
//! would have shipped as `Other`. The rule below is therefore "later
//! patterns are more specific, EXCEPT that an unclassifiable capture never
//! displaces a classified one" — see [`map_class`]'s call site.
//!
//! ## Encoding
//!
//! `store::Store::put_highlights` serializes `Vec<Span>` as JSON
//! (`serde_json`) into the `highlights.spans` `BLOB` column — the simpler
//! of the two options the design brief allowed ("a compact binary or JSON
//! encoding"). The column is opaque, so swapping to a tighter binary
//! encoding later needs no schema migration.
//!
//! ## Overlap resolution
//!
//! Tree-sitter highlight captures are almost always leaf tokens
//! (identifiers, string literals, keyword tokens, ...) that don't nest, but
//! the SAME node can be captured by more than one pattern (a general
//! `@variable` rule and a more specific `@variable.parameter` rule
//! matching identically). We keep the LAST class seen for an EXACT
//! `(start, end)` duplicate (later patterns in `highlights.scm` are
//! conventionally more specific), then run one left-to-right greedy sweep:
//! spans are processed in `(start, end)` order, an earlier span wins
//! outright, and any later span starting before the cursor is clipped to
//! start where the previous span ended (or dropped if that empties it).
//! This guarantees a non-overlapping, in-bounds result; it is NOT full
//! nested-highlight priority stacking (`tree-sitter-highlight`'s job,
//! which this Wave doesn't need).
//!
//! ## Injections (V72-H2a)
//!
//! [`extract_highlights`] also paints every guest region
//! `crate::injection` declares for the file's language — a Markdown
//! fence's Ruby, painted with the Ruby query and shifted into Markdown
//! coordinates. [`extract_highlights_host_only`] is the same pass WITHOUT
//! that step, and is what the injection layer itself calls, which is what
//! bounds painting at exactly one level.

use crate::lang::{self, LangError};
use crate::syntax::{self, SyntaxRow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use tree_sitter::StreamingIterator;

pub type Result<T> = std::result::Result<T, LangError>;

/// The ROLE TABLE version, embedded in every
/// `lang::LangInfo::highlight_salt` (pinned by
/// `lang::tests::the_two_salt_families_are_disjoint_and_role_versioned`).
///
/// `1` = the fifteen classes shipped from W1 through v7.1. `2` = V72-H2b's
/// eighteen. Bump it in the SAME edit that adds, removes or re-buckets a
/// [`HighlightClass`], and bump every `highlight_salt` with it: cached
/// spans carry class values, so a re-bucketing that leaves the salts alone
/// serves rows painted under the old vocabulary forever.
pub const ROLE_TABLE_VERSION: u32 = 2;

/// Every role, in wire order — the vocabulary `kbc-theme/1` binds to.
/// Declared as strings beside the enum (rather than derived from it) for
/// exactly one reason: it is the list the SPA mirrors
/// (`web-code/src/api/types.ts`, `themes/derive.ts`'s `SYNTAX_ROLES`,
/// `styles/tokens.css`'s `--syn-*`), and a lock-step contract needs a
/// literal on each side. `roles_match_the_serialized_class_names` pins it
/// to the enum's own serde output, so the two can never drift.
pub const ROLES: &[&str] = &[
    "keyword",
    "string",
    "string-special",
    "comment",
    "function",
    "type",
    "number",
    "variable",
    "constant",
    "constant-builtin",
    "operator",
    "punctuation",
    "punctuation-special",
    "property",
    "attribute",
    "label",
    "escape",
    "other",
];

/// The eighteen highlight roles. `kebab-case` on the wire: every one of
/// the fifteen pre-V72-H2b members is a single word and serializes
/// byte-identically under `kebab-case` and `snake_case`
/// (`the_fifteen_legacy_roles_serialize_byte_identically` pins that), and
/// the three new members read better as `constant-builtin` than
/// `constant_builtin` in a CSS class and a custom property.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HighlightClass {
    Keyword,
    String,
    /// V72-H2b — `string.special` and its sub-scopes: a Ruby symbol or
    /// regex literal, a JS template/regex, a JSON object KEY, a CSS/TOML
    /// url. Not a plain string, and reading a JSON file where the keys and
    /// the values are one colour is the case that made this a role.
    StringSpecial,
    Comment,
    Function,
    Type,
    Number,
    Variable,
    Constant,
    /// V72-H2b — `constant.builtin` (`nil`, `None`, `true`, `self`,
    /// `null`) plus YAML/TOML's top-level `boolean`. The language owns
    /// these; a user constant is a different thing.
    ConstantBuiltin,
    Operator,
    Punctuation,
    /// V72-H2b — `punctuation.special`: interpolation delimiters (`#{`,
    /// `${`), YAML's directive/document markers, Markdown's block markers.
    PunctuationSpecial,
    Property,
    Attribute,
    Label,
    Escape,
    /// Any `highlights.scm` scope not in the fixed set above (e.g.
    /// `embedded`, Markdown's `text.*`) — kept rather than silently
    /// dropped.
    Other,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub byte_start: u32,
    pub byte_len: u32,
    pub class: HighlightClass,
}

/// Highlight spans for `source`, INCLUDING every injected guest region
/// (V72-H2a): a Markdown fence's Ruby is painted with the Ruby query and
/// re-anchored into Markdown coordinates by `crate::injection`.
///
/// This is the entry point every caller wants. [`extract_highlights_host_only`]
/// is the one the injection layer itself calls, and the reason nothing
/// recurses — see `injection`'s module doc.
pub fn extract_highlights(lang_id: &str, source: &[u8]) -> Result<Vec<Span>> {
    let host = extract_highlights_host_only(lang_id, source)?;
    // HAML's own scanner already paints its Ruby fragments through the
    // SAME injection layer (it has the scanned document in hand and would
    // otherwise re-walk it), so its host-only result is already complete.
    if lang_id == "haml" || !crate::injection::is_host(lang_id) {
        return Ok(host);
    }
    Ok(crate::injection::paint(lang_id, source, host))
}

/// The HOST language's own spans, with no injected guest regions.
///
/// Structurally never reaches `crate::injection`, which is what bounds
/// injection painting at exactly one level (`injection`'s module doc). A
/// caller outside that module almost certainly wants
/// [`extract_highlights`].
pub fn extract_highlights_host_only(lang_id: &str, source: &[u8]) -> Result<Vec<Span>> {
    // PRR-N3 — ERB has no `highlights.scm` vendored in this crate (see
    // `lang::ERB`'s doc; syntax highlighting for `.erb` is out of this
    // lens's scope). Short-circuit the same way `extract::extract_symbols`
    // does, for the same reason: `ingest::index_file` calls this
    // unconditionally for every detected language, and an `Unsupported`
    // error here would abort the whole repo walk on the first `.erb` file.
    // V72-H2a note: ERB is an injection HOST, so the layer could paint its
    // Ruby fragments — but it would be painting them onto nothing, since
    // there is no `erb` highlights query for the template itself, and the
    // row's tier is `none`, so `ingest::index_file`'s plan never asks for
    // spans at all. Widening ERB's tier is D7's Herb decision, not this
    // unit's; the short-circuit stays.
    if lang_id == "erb" {
        return Ok(Vec::new());
    }
    // V72-H3 (D7) — HAML's spans come from `crate::haml`'s own scanner
    // (template tokens) PLUS this module's Ruby query run over every Ruby
    // fragment the scanner found, shifted into HAML coordinates. There is
    // no `haml` grammar and no `haml-highlights.scm`; the tier's highlight
    // promise is backed by `syntax/1`'s `Engine::Scanner` instead.
    if lang_id == "haml" {
        return Ok(crate::haml::highlights(source));
    }
    let (tree, language) = lang::parse(lang_id, source)?;
    let hl_src = lang::highlights_query(lang_id)
        .ok_or_else(|| LangError::Unsupported(lang_id.to_string()))?;
    let query = lang::compile_query(lang_id, &language, &hl_src)?;
    let capture_names = query.capture_names();

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut captures = cursor.captures(&query, tree.root_node(), source);

    // Dedup exact-range duplicates: last-write-wins (BTreeMap::insert
    // overwrites), and BTreeMap's key ordering (start ASC, end ASC) is
    // exactly the order the sweep below needs — no separate sort.
    let mut by_range: BTreeMap<(usize, usize), HighlightClass> = BTreeMap::new();
    while let Some((m, capture_index)) = captures.next() {
        let cap = m.captures[*capture_index];
        let cname = capture_names[cap.index as usize];
        // V72-H2b: the FULL capture name, not its top-level word —
        // `map_class` owns the two-level lookup so there is exactly one
        // place that decides how specific a scope is allowed to be.
        let class = map_class(cname);
        // Last-write-wins, EXCEPT that an unclassifiable capture never
        // displaces a classified one — see the module doc's `@spell`
        // paragraph for the case that made this necessary.
        by_range
            .entry((cap.node.start_byte(), cap.node.end_byte()))
            .and_modify(|slot| {
                if class != HighlightClass::Other {
                    *slot = class;
                }
            })
            .or_insert(class);
    }

    let mut spans = Vec::with_capacity(by_range.len());
    let mut sweep_pos: usize = 0;
    for ((start, end), class) in by_range {
        let clipped_start = start.max(sweep_pos);
        if clipped_start >= end {
            continue; // fully swallowed by a previously emitted span
        }
        spans.push(Span {
            byte_start: clipped_start as u32,
            byte_len: (end - clipped_start) as u32,
            class,
        });
        sweep_pos = end;
    }
    Ok(spans)
}

/// The first TWO dotted scope words of `cname`, and the first alone —
/// `map_class`'s two lookup keys, in priority order. Pure and total: a
/// capture with no dot yields the same string twice, and the specific
/// lookup simply misses.
fn scope_keys(cname: &str) -> (&str, &str) {
    let top = cname.split('.').next().unwrap_or(cname);
    let two = match cname[top.len()..].strip_prefix('.') {
        Some(rest) => {
            let second = rest.split('.').next().unwrap_or(rest);
            &cname[..top.len() + 1 + second.len()]
        }
        None => top,
    };
    (two, top)
}

/// Bucket a capture name into a role. V72-H2b: the SPECIFIC (two-word)
/// scope wins when it is listed, else the top-level word decides exactly as
/// it did before — so every unlisted sub-scope keeps its historical class.
fn map_class(cname: &str) -> HighlightClass {
    let (specific, top) = scope_keys(cname);
    match specific {
        // V72-H2b (D16) — the three widened roles. Every other dotted
        // scope falls through to the top-level match below.
        "constant.builtin" => return HighlightClass::ConstantBuiltin,
        "punctuation.special" => return HighlightClass::PunctuationSpecial,
        "string.special" => return HighlightClass::StringSpecial,
        _ => {}
    }
    match top {
        "keyword" => HighlightClass::Keyword,
        "string" => HighlightClass::String,
        "comment" => HighlightClass::Comment,
        "function" | "constructor" => HighlightClass::Function,
        // V72-H2a — CSS's `tag` is an element selector (`div`, `a`, `*`,
        // `&`). It reads as a type name, which is also where HAML's
        // scanner already paints a tag name.
        "type" | "tag" => HighlightClass::Type,
        "number" => HighlightClass::Number,
        "variable" => HighlightClass::Variable,
        "constant" => HighlightClass::Constant,
        // V72-H2b — YAML/TOML's `boolean` (`true`/`false`/`~`/`null`) is a
        // BUILTIN constant; it rode `Constant` only for as long as there
        // was no honest bucket for it. See the module doc.
        "boolean" => HighlightClass::ConstantBuiltin,
        "operator" => HighlightClass::Operator,
        "punctuation" => HighlightClass::Punctuation,
        "property" => HighlightClass::Property,
        "attribute" => HighlightClass::Attribute,
        "label" => HighlightClass::Label,
        "escape" => HighlightClass::Escape,
        _ => HighlightClass::Other,
    }
}

// ── V76-C1 — `highlight/1`: paint ANY snippet, nothing persisted ──────────

/// Wire schema for a single snippet.
pub const HIGHLIGHT_SCHEMA: &str = "highlight/1";
/// Wire schema for the batch form.
pub const HIGHLIGHT_BATCH_SCHEMA: &str = "highlight-batch/1";
/// One snippet may be at most this many UTF-8 bytes. Oversize is a 400
/// naming the size, never a silent truncate.
pub const MAX_SNIPPET_BYTES: usize = 256 * 1024;
/// A batch may carry at most this many items.
pub const MAX_BATCH_ITEMS: usize = 64;
/// Sum of every item's `text` in a batch, UTF-8 bytes.
pub const MAX_BATCH_BYTES: usize = 1024 * 1024;

/// One role span, line-relative. `line` is 1-based; `start`/`end` are
/// 0-based UTF-8 byte columns within that line (tree-sitter `Point.column`
/// convention, exclusive end). The newline itself is never a column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRoleSpan {
    pub line: u32,
    pub start: u32,
    pub end: u32,
    pub role: HighlightClass,
}

/// What this paint is, and what it is not. Same four fields `outline/1`
/// carries: a `none`-tier language is an honest empty result, never a 500.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HighlightHonesty {
    pub tier: &'static str,
    pub engine: String,
    /// `highlights` when the extractor ran; `none` when the type does not
    /// paint (unknown, named-but-unparsed, or a parse-only grammar).
    pub derived_from: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `highlight/1` response. Computed per request, never stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HighlightOut {
    pub schema: &'static str,
    pub lang: Option<&'static str>,
    pub tier: &'static str,
    pub spans: Vec<LineRoleSpan>,
    pub honesty: HighlightHonesty,
    /// Present only when the request asked `salt: true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salt: Option<&'static str>,
}

/// `POST /api/highlight` body.
#[derive(Debug, Clone, Deserialize)]
pub struct HighlightIn {
    /// A `syntax/1` language id, a fence alias (`rb`, `ts`, …), or `null`
    /// to infer from [`path`].
    pub lang: Option<String>,
    /// Used only when `lang` is absent/empty: `syntax/1` detection.
    pub path: Option<String>,
    pub text: String,
    #[serde(default)]
    pub salt: bool,
}

/// One item in `POST /api/highlight/batch`.
#[derive(Debug, Clone, Deserialize)]
pub struct HighlightBatchItemIn {
    pub id: String,
    pub lang: Option<String>,
    pub path: Option<String>,
    pub text: String,
}

/// `POST /api/highlight/batch` body.
#[derive(Debug, Clone, Deserialize)]
pub struct HighlightBatchIn {
    pub items: Vec<HighlightBatchItemIn>,
}

/// One painted item in a batch response — the snippet's own `highlight/1`
/// body plus the caller-supplied `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HighlightBatchItemOut {
    pub id: String,
    pub schema: &'static str,
    pub lang: Option<&'static str>,
    pub tier: &'static str,
    pub spans: Vec<LineRoleSpan>,
    pub honesty: HighlightHonesty,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salt: Option<&'static str>,
}

/// `highlight-batch/1` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HighlightBatchOut {
    pub schema: &'static str,
    pub items: Vec<HighlightBatchItemOut>,
}

impl HighlightBatchItemOut {
    fn from_out(id: String, out: HighlightOut) -> Self {
        Self {
            id,
            schema: out.schema,
            lang: out.lang,
            tier: out.tier,
            spans: out.spans,
            honesty: out.honesty,
            salt: out.salt,
        }
    }
}

fn engine_label(row: Option<&SyntaxRow>) -> String {
    match row.map(|r| r.engine) {
        Some(syntax::Engine::TreeSitter(g)) => format!("tree-sitter:{g}"),
        Some(syntax::Engine::Scanner(s)) => format!("scanner:{s}"),
        _ => "none".to_string(),
    }
}

/// Resolve a caller-supplied language token to a registry row.
///
/// Exact `syntax/1` id first (`row_for_lang`), then a Markdown fence
/// alias (`rb` → `ruby`) via `markdown::resolve_info_string`. An unknown
/// token is `None` — the caller paints `tier: none`, never 500s.
fn row_for_lang_or_alias(token: &str) -> Option<&'static SyntaxRow> {
    let t = token.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(row) = syntax::row_for_lang(t) {
        return Some(row);
    }
    crate::markdown::resolve_info_string(t).and_then(syntax::row_for_lang)
}

/// Pick the registry row for a snippet. `lang` wins when it is a non-empty
/// token; otherwise `path` (plus the snippet bytes, for a shebang) decides.
fn resolve_row<'a>(
    lang: Option<&'a str>,
    path: Option<&'a str>,
    text: &[u8],
) -> std::result::Result<Option<&'static SyntaxRow>, String> {
    if let Some(token) = lang.map(str::trim).filter(|s| !s.is_empty()) {
        return match row_for_lang_or_alias(token) {
            Some(row) => Ok(Some(row)),
            None => Err(format!(
                "unknown language {token:?} — not a syntax/1 id or a fence alias this build paints"
            )),
        };
    }
    if let Some(p) = path.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(syntax::row_for_path(p, Some(text)));
    }
    Err("lang is null and no path to infer from — pass a syntax/1 id or a path".into())
}

/// Bucket byte-offset [`Span`]s onto 1-based lines as UTF-8 columns.
/// Multi-line spans split at line boundaries; the newline is never a column.
pub fn line_spans_from_bytes(source: &[u8], spans: &[Span]) -> Vec<LineRoleSpan> {
    if source.is_empty() || spans.is_empty() {
        return Vec::new();
    }
    let mut starts: Vec<u32> = vec![0];
    for (i, &b) in source.iter().enumerate() {
        if b == b'\n' {
            starts.push((i + 1) as u32);
        }
    }
    let total = source.len() as u32;
    let line_index = |byte: u32| -> usize {
        let mut lo = 0usize;
        let mut hi = starts.len() - 1;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if starts[mid] <= byte {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    };
    let mut out = Vec::new();
    for span in spans {
        if span.byte_len == 0 {
            continue;
        }
        let span_start = span.byte_start;
        let span_end = span.byte_start.saturating_add(span.byte_len);
        if span_end <= span_start || span_start >= total {
            continue;
        }
        let clipped_start = span_start;
        let clipped_end = span_end.min(total);
        let first = line_index(clipped_start);
        let last = line_index(clipped_end.saturating_sub(1));
        for li in first..=last {
            let line_byte_start = starts[li];
            let line_content_end = starts
                .get(li + 1)
                .map(|n| n.saturating_sub(1))
                .unwrap_or(total);
            let overlap_start = clipped_start.max(line_byte_start);
            let overlap_end = clipped_end.min(line_content_end);
            if overlap_end <= overlap_start {
                continue;
            }
            out.push(LineRoleSpan {
                line: (li as u32) + 1,
                start: overlap_start - line_byte_start,
                end: overlap_end - line_byte_start,
                role: span.class,
            });
        }
    }
    out
}

fn empty_out(row: Option<&'static SyntaxRow>, reason: String, want_salt: bool) -> HighlightOut {
    let tier = row.map(|r| r.tier.as_str()).unwrap_or("none");
    HighlightOut {
        schema: HIGHLIGHT_SCHEMA,
        lang: row.map(|r| r.lang),
        tier,
        spans: Vec::new(),
        honesty: HighlightHonesty {
            tier,
            engine: engine_label(row),
            derived_from: "none",
            reason: Some(reason),
        },
        salt: if want_salt {
            row.and_then(|r| r.info).map(|i| i.highlight_salt)
        } else {
            None
        },
    }
}

fn refuse_size(kind: &str, got: usize, cap: usize, cap_label: &str) -> crate::routes::ApiError {
    crate::routes::ApiError::bad_request(format!(
        "{kind} is {got} bytes; highlight/1 refuses above {cap} ({cap_label}) — shrink the snippet"
    ))
}

/// Paint one snippet. Caps refuse with a 400 naming the size; an unknown
/// or `none`-tier language is a 200 with empty spans and a reason.
pub fn highlight_snippet(
    req: &HighlightIn,
) -> std::result::Result<HighlightOut, crate::routes::ApiError> {
    let n = req.text.len();
    if n > MAX_SNIPPET_BYTES {
        return Err(refuse_size("text", n, MAX_SNIPPET_BYTES, "256 KiB"));
    }
    let bytes = req.text.as_bytes();
    let row = match resolve_row(req.lang.as_deref(), req.path.as_deref(), bytes) {
        Ok(row) => row,
        Err(reason) => return Ok(empty_out(None, reason, req.salt)),
    };
    let Some(row) = row else {
        return Ok(empty_out(
            None,
            "no syntax/1 registry row for this file type".into(),
            req.salt,
        ));
    };
    if !row.plan().highlight {
        let reason = row
            .note
            .unwrap_or("this file type's tier derives no highlight spans")
            .to_string();
        return Ok(empty_out(Some(row), reason, req.salt));
    }
    let spans = match extract_highlights(row.lang, bytes) {
        Ok(s) => line_spans_from_bytes(bytes, &s),
        Err(e) => {
            return Ok(empty_out(
                Some(row),
                format!("extractor refused: {e}"),
                req.salt,
            ));
        }
    };
    Ok(HighlightOut {
        schema: HIGHLIGHT_SCHEMA,
        lang: Some(row.lang),
        tier: row.tier.as_str(),
        spans,
        honesty: HighlightHonesty {
            tier: row.tier.as_str(),
            engine: engine_label(Some(row)),
            derived_from: "highlights",
            reason: None,
        },
        salt: if req.salt {
            row.info.map(|i| i.highlight_salt)
        } else {
            None
        },
    })
}

/// Paint a page of snippets. Caps refuse with numbers; each item is
/// otherwise independent (one unknown language does not 500 the batch).
pub fn highlight_batch(
    req: &HighlightBatchIn,
) -> std::result::Result<HighlightBatchOut, crate::routes::ApiError> {
    let n = req.items.len();
    if n == 0 {
        return Err(crate::routes::ApiError::bad_request(
            "items is empty; send at least one snippet",
        ));
    }
    if n > MAX_BATCH_ITEMS {
        return Err(crate::routes::ApiError::bad_request(format!(
            "batch has {n} items; highlight/1 refuses above {MAX_BATCH_ITEMS} — split the request"
        )));
    }
    let mut seen = HashSet::with_capacity(n);
    let mut total = 0usize;
    for item in &req.items {
        if item.id.is_empty() {
            return Err(crate::routes::ApiError::bad_request(
                "every batch item needs a non-empty id",
            ));
        }
        if !seen.insert(item.id.as_str()) {
            return Err(crate::routes::ApiError::bad_request(format!(
                "duplicate batch id {:?} — ids must be unique in one request",
                item.id
            )));
        }
        total = total.saturating_add(item.text.len());
        if item.text.len() > MAX_SNIPPET_BYTES {
            return Err(refuse_size(
                &format!("item {:?} text", item.id),
                item.text.len(),
                MAX_SNIPPET_BYTES,
                "256 KiB",
            ));
        }
    }
    if total > MAX_BATCH_BYTES {
        return Err(crate::routes::ApiError::bad_request(format!(
            "batch total is {total} bytes; highlight/1 refuses above {MAX_BATCH_BYTES} (1 MiB) — split the request"
        )));
    }
    let mut items = Vec::with_capacity(n);
    for item in &req.items {
        let out = highlight_snippet(&HighlightIn {
            lang: item.lang.clone(),
            path: item.path.clone(),
            text: item.text.clone(),
            salt: false,
        })?;
        items.push(HighlightBatchItemOut::from_out(item.id.clone(), out));
    }
    Ok(HighlightBatchOut {
        schema: HIGHLIGHT_BATCH_SCHEMA,
        items,
    })
}

// ── routes ───────────────────────────────────────────────────────────────

use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;

/// `POST /api/highlight` — paint one snippet. Bearer read; nothing persisted.
pub async fn highlight_route(
    Json(body): Json<HighlightIn>,
) -> std::result::Result<impl IntoResponse, crate::routes::ApiError> {
    let out = highlight_snippet(&body)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `POST /api/highlight/batch` — paint up to [`MAX_BATCH_ITEMS`] snippets.
pub async fn highlight_batch_route(
    Json(body): Json<HighlightBatchIn>,
) -> std::result::Result<impl IntoResponse, crate::routes::ApiError> {
    let out = highlight_batch(&body)?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

fn no_query_params_accept_without(_omit: &str) -> bool {
    true
}

pub const HIGHLIGHT_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/highlight",
    handler: "highlight::highlight_route",
    required_params: &[],
    params_accept_without: no_query_params_accept_without,
};

pub const HIGHLIGHT_BATCH_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/highlight/batch",
    handler: "highlight::highlight_batch_route",
    required_params: &[],
    params_accept_without: no_query_params_accept_without,
};

/// Every route V76-C1 adds. Walked from BOTH sides (this crate against
/// `router.rs`, kb-code-cli against the verbs) — see
/// `entities::V71_G0_ROUTES`. Required-params is empty: the contract is a
/// JSON body, enforced by axum's `Json` extractor.
pub const V76_C1_ROUTES: &[crate::entities::RouteContract] =
    &[HIGHLIGHT_ROUTE, HIGHLIGHT_BATCH_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const RUST_SNIPPET: &str = "fn add(a: i32, b: i32) -> i32 {\n    // sum\n    a + b\n}\n";
    const PYTHON_SNIPPET: &str = "def greet(name):\n    # say hi\n    return f\"hi {name}\"\n";
    const RUBY_SNIPPET: &str = "def greet(name)\n  # say hi\n  \"hi #{name}\"\nend\n";
    const TYPESCRIPT_SNIPPET: &str =
        "function add(a: number, b: number): number {\n    // sum\n    return a + b;\n}\n";
    const TSX_SNIPPET: &str = "function Hello(props: { name: string }) {\n    // a component\n    return <div>{props.name}</div>;\n}\n";
    const JAVASCRIPT_SNIPPET: &str = "function add(a, b) {\n    // sum\n    return a + b;\n}\n";
    const BASH_SNIPPET: &str = "greet() {\n  # say hi\n  echo \"hi $1\"\n}\n";
    const YAML_SNIPPET: &str = "# a comment\nname: web\nreplicas: 3\nenabled: true\n";
    const GO_SNIPPET: &str =
        "// add sums two ints\nfunc add(a int, b int) int {\n\treturn a + b\n}\n";
    const TOML_SNIPPET: &str = "# a comment\n[package]\nname = \"kb-code\"\nversion = 1\n";
    const JSON_SNIPPET: &str = "{\n  \"name\": \"kb-code\",\n  \"count\": 3\n}\n";
    const CSS_SNIPPET: &str = "/* tokens */\n.btn {\n  --radius: 4px;\n  color: #336699;\n}\n";
    const SCSS_SNIPPET: &str =
        "$brand: #336699;\n@mixin b($s) {\n  padding: $s;\n}\n.card { color: $brand; }\n";
    const MARKDOWN_SNIPPET: &str = "# Title\n\nSome prose.\n\n```ruby\nclass A\nend\n```\n";

    #[test]
    fn rust_small_fixture_highlight_spans_snapshot() {
        let spans = extract_highlights("rust", RUST_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    #[test]
    fn go_small_fixture_highlight_spans_snapshot() {
        let spans = extract_highlights("go", GO_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    // ── V72-H2a — the three new grammars, plus the injection lane ────────

    #[test]
    fn css_small_fixture_highlight_spans_snapshot() {
        let spans = extract_highlights("css", CSS_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    #[test]
    fn scss_small_fixture_highlight_spans_snapshot() {
        let spans = extract_highlights("scss", SCSS_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    /// The Markdown fixture's own block spans PLUS the Ruby inside its
    /// fence, re-anchored into Markdown coordinates by `crate::injection`.
    /// The golden is the proof the offsets are the fence body's, not the
    /// guest's own.
    #[test]
    fn markdown_small_fixture_with_an_injected_fence_snapshot() {
        let spans = extract_highlights("markdown", MARKDOWN_SNIPPET.as_bytes()).unwrap();
        insta::assert_debug_snapshot!(spans);
    }

    /// The host-only entry point is what the injection layer calls, and
    /// the reason nothing recurses. For a host it is strictly a SUBSET of
    /// the full result; for every other language the two are identical.
    #[test]
    fn host_only_and_full_differ_exactly_by_the_injected_regions() {
        let host_only =
            extract_highlights_host_only("markdown", MARKDOWN_SNIPPET.as_bytes()).unwrap();
        let full = extract_highlights("markdown", MARKDOWN_SNIPPET.as_bytes()).unwrap();
        assert!(
            full.len() > host_only.len(),
            "the fence must contribute spans: {} vs {}",
            full.len(),
            host_only.len()
        );
        for lang in ["rust", "ruby", "css", "scss", "yaml"] {
            let src = match lang {
                "rust" => RUST_SNIPPET,
                "ruby" => RUBY_SNIPPET,
                "css" => CSS_SNIPPET,
                "scss" => SCSS_SNIPPET,
                _ => YAML_SNIPPET,
            };
            assert_eq!(
                extract_highlights(lang, src.as_bytes()).unwrap(),
                extract_highlights_host_only(lang, src.as_bytes()).unwrap(),
                "{lang}: a non-host language must be unaffected by the injection layer"
            );
        }
    }

    /// HAML paints its Ruby through the same layer but from its own
    /// scanner entry point (it has the document in hand); the full and
    /// host-only results must therefore be IDENTICAL, or the layer would
    /// be painting it twice.
    /// The dedup rule V72-H2a added: SCSS captures its `//` comments as
    /// `@comment @spell`, and the unmapped one must not win.
    #[test]
    fn an_unclassifiable_capture_never_displaces_a_classified_one() {
        let spans = extract_highlights("scss", b"// a line comment\n$a: 1;\n").unwrap();
        let first = spans.first().expect("the comment produces a span");
        assert_eq!(
            first.class,
            HighlightClass::Comment,
            "an SCSS line comment must stay a Comment: {spans:?}"
        );
    }

    #[test]
    fn haml_is_not_painted_twice() {
        let src = b"%section#hero\n  %p= t('.title')\n";
        assert_eq!(
            extract_highlights("haml", src).unwrap(),
            extract_highlights_host_only("haml", src).unwrap()
        );
    }

    fn assert_nonoverlapping_and_inbounds(spans: &[Span], source_len: usize) {
        let mut prev_end: u32 = 0;
        for (i, s) in spans.iter().enumerate() {
            assert!(s.byte_len > 0, "span {i} has zero length: {s:?}");
            let end = s.byte_start + s.byte_len;
            assert!(
                (end as usize) <= source_len,
                "span {i} out of bounds: {s:?} vs source_len={source_len}"
            );
            assert!(
                s.byte_start >= prev_end,
                "span {i} overlaps the previous span: {s:?}, prev_end={prev_end}"
            );
            prev_end = end;
        }
    }

    #[test]
    fn highlight_spans_are_nonoverlapping_and_inbounds() {
        for (lang, src) in [
            ("rust", RUST_SNIPPET),
            ("python", PYTHON_SNIPPET),
            ("ruby", RUBY_SNIPPET),
            ("typescript", TYPESCRIPT_SNIPPET),
            ("tsx", TSX_SNIPPET),
            ("javascript", JAVASCRIPT_SNIPPET),
            ("bash", BASH_SNIPPET),
            ("yaml", YAML_SNIPPET),
            ("go", GO_SNIPPET),
            ("toml", TOML_SNIPPET),
            ("json", JSON_SNIPPET),
            ("css", CSS_SNIPPET),
            ("scss", SCSS_SNIPPET),
            ("markdown", MARKDOWN_SNIPPET),
        ] {
            let spans = extract_highlights(lang, src.as_bytes()).unwrap();
            assert!(!spans.is_empty(), "{lang} produced no spans at all");
            assert_nonoverlapping_and_inbounds(&spans, src.len());
        }
    }

    #[test]
    fn empty_source_yields_no_spans() {
        for lang in lang::ALL_LANG_IDS {
            assert_eq!(extract_highlights(lang, b"").unwrap(), vec![]);
        }
    }

    #[test]
    fn unsupported_language_errors() {
        let err = extract_highlights("cobol", b"").unwrap_err();
        assert!(matches!(err, LangError::Unsupported(_)), "got: {err:?}");
    }

    #[test]
    fn map_class_covers_every_observed_top_level_scope() {
        // Every top-level scope word actually present in the eight bundled
        // highlights.scm files (grepped at authoring time — TypeScript's is
        // concatenated with JavaScript's, see lang.rs) must map to
        // something other than the catch-all `Other`, EXCEPT `embedded`
        // (an injection marker, not a real highlight — see module doc).
        let observed = [
            "attribute",
            "boolean",
            "comment",
            "constant",
            "constructor",
            "escape",
            "function",
            "keyword",
            "label",
            "number",
            "operator",
            "property",
            "punctuation",
            "string",
            "type",
            "variable",
        ];
        for scope in observed {
            assert_ne!(
                map_class(scope),
                HighlightClass::Other,
                "scope {scope:?} unexpectedly fell through to Other"
            );
        }
        // V72-H2a — the three new grammars' own queries add two more
        // top-level scopes, both of which must land somewhere real.
        // V72-H2a — CSS's element-selector scope joins the mapped set.
        assert_eq!(map_class("tag"), HighlightClass::Type);
        // Markdown's and SCSS's remaining scopes stay `Other` on purpose:
        // a heading, a URI and a spell-check marker have no honest home in
        // the FIXED 15-class set. Pinned so the choice is a decision on
        // record rather than an oversight.
        for scope in ["text", "none", "spell"] {
            assert_eq!(
                map_class(scope),
                HighlightClass::Other,
                "scope {scope:?} is deliberately unclassified — see the module doc"
            );
        }
        assert_eq!(map_class("embedded"), HighlightClass::Other);
        assert_eq!(map_class("something-unheard-of"), HighlightClass::Other);
    }

    // ── V72-H2b (D16) — the eighteen-role table ──────────────────────────

    /// Every capture name the queries in THIS BUILD emit, per language id.
    /// The evidence the widening was picked from, read from the grammars
    /// rather than restated from a note.
    fn capture_names_for(lang_id: &str) -> Vec<String> {
        let src = lang::highlights_query(lang_id).expect("a highlights query");
        // `lang::parse` is the public door to the compiled grammar (it
        // hands back the `Language` beside the tree); parsing empty bytes
        // is the cheapest way through it.
        let (_tree, language) = lang::parse(lang_id, b"").expect("a grammar");
        let query = lang::compile_query(lang_id, &language, &src).expect("compiles");
        query
            .capture_names()
            .iter()
            .map(|n| (*n).to_string())
            .collect()
    }

    /// How many of this build's grammars emit a capture whose two-level
    /// scope key is `key`.
    fn grammars_emitting(key: &str) -> usize {
        lang::ALL_LANG_IDS
            .iter()
            .filter(|id| capture_names_for(id).iter().any(|n| scope_keys(n).0 == key))
            .count()
    }

    /// The three roles D16 adopted, and the exact coverage that picked
    /// them. A grammar bump that moves one of these numbers is precisely
    /// when the choice deserves re-reading — so the numbers are asserted,
    /// not narrated.
    #[test]
    fn every_widened_role_is_reachable_from_the_queries_in_this_build() {
        let adopted = [
            ("constant.builtin", 9usize, HighlightClass::ConstantBuiltin),
            ("punctuation.special", 7, HighlightClass::PunctuationSpecial),
            ("string.special", 7, HighlightClass::StringSpecial),
        ];
        for (key, expected, class) in adopted {
            let n = grammars_emitting(key);
            assert_eq!(
                n, expected,
                "{key}: {n} grammar(s) emit it, the module doc's table says {expected} —                  update the table (and re-read the choice) in the same edit"
            );
            assert_eq!(map_class(key), class);
        }
    }

    /// MEASURED AND NOT ADOPTED — the `usages2::UNMINTED_KINDS` precedent.
    /// Each candidate keeps its historical class, and its coverage is
    /// pinned so "we looked" stays true rather than becoming folklore.
    #[test]
    fn deferred_role_candidates_are_recorded_with_their_evidence() {
        let deferred = [
            ("constructor", 6usize, HighlightClass::Function),
            ("variable.parameter", 5, HighlightClass::Variable),
            ("type.builtin", 3, HighlightClass::Type),
            ("namespace", 0, HighlightClass::Other),
        ];
        for (key, expected, folds_into) in deferred {
            let n = grammars_emitting(key);
            assert_eq!(
                n, expected,
                "{key}: {n} grammar(s) emit it, the module doc's table says {expected}"
            );
            assert_eq!(
                map_class(key),
                folds_into,
                "{key} is deferred — it must keep folding into its historical class"
            );
        }
        // Markdown's `text.*` family: one grammar, four captures.
        let md = capture_names_for("markdown");
        assert_eq!(
            md.iter().filter(|n| scope_keys(n).1 == "text").count(),
            4,
            "markdown's text.* family: {md:?}"
        );
        for n in &md {
            if scope_keys(n).1 == "text" {
                assert_eq!(map_class(n), HighlightClass::Other);
            }
        }
    }

    /// [`ROLES`] is what the SPA mirrors; the enum is what the wire
    /// carries. They are two literals, so they get one test.
    #[test]
    fn roles_match_the_serialized_class_names() {
        let all = [
            HighlightClass::Keyword,
            HighlightClass::String,
            HighlightClass::StringSpecial,
            HighlightClass::Comment,
            HighlightClass::Function,
            HighlightClass::Type,
            HighlightClass::Number,
            HighlightClass::Variable,
            HighlightClass::Constant,
            HighlightClass::ConstantBuiltin,
            HighlightClass::Operator,
            HighlightClass::Punctuation,
            HighlightClass::PunctuationSpecial,
            HighlightClass::Property,
            HighlightClass::Attribute,
            HighlightClass::Label,
            HighlightClass::Escape,
            HighlightClass::Other,
        ];
        let wire: Vec<String> = all
            .iter()
            .map(|c| {
                serde_json::to_value(c)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(wire, ROLES, "ROLES must be the serde names, in wire order");
        assert_eq!(ROLES.len(), 18, "D16's budget is eighteen roles");
        assert_eq!(ROLE_TABLE_VERSION, 2);
    }

    /// The `snake_case` → `kebab-case` switch is byte-identical for every
    /// role that existed before V72-H2b — the reason it was safe to make.
    #[test]
    fn the_fifteen_legacy_roles_serialize_byte_identically() {
        for (class, legacy) in [
            (HighlightClass::Keyword, "keyword"),
            (HighlightClass::String, "string"),
            (HighlightClass::Comment, "comment"),
            (HighlightClass::Function, "function"),
            (HighlightClass::Type, "type"),
            (HighlightClass::Number, "number"),
            (HighlightClass::Variable, "variable"),
            (HighlightClass::Constant, "constant"),
            (HighlightClass::Operator, "operator"),
            (HighlightClass::Punctuation, "punctuation"),
            (HighlightClass::Property, "property"),
            (HighlightClass::Attribute, "attribute"),
            (HighlightClass::Label, "label"),
            (HighlightClass::Escape, "escape"),
            (HighlightClass::Other, "other"),
        ] {
            assert_eq!(
                serde_json::to_value(class).unwrap(),
                serde_json::json!(legacy)
            );
        }
    }

    /// The two-level lookup promotes ONLY what is listed; every other
    /// dotted scope keeps the class its top-level word always gave it.
    #[test]
    fn the_two_level_lookup_only_promotes_listed_sub_scopes() {
        assert_eq!(
            scope_keys("string.special.regex"),
            ("string.special", "string")
        );
        assert_eq!(scope_keys("comment"), ("comment", "comment"));
        assert_eq!(
            scope_keys("function.method.builtin"),
            ("function.method", "function")
        );
        for (cname, class) in [
            ("comment.documentation", HighlightClass::Comment),
            ("string.escape", HighlightClass::String),
            ("function.macro", HighlightClass::Function),
            ("function.method.builtin", HighlightClass::Function),
            ("punctuation.bracket", HighlightClass::Punctuation),
            ("punctuation.delimiter", HighlightClass::Punctuation),
            ("variable.builtin", HighlightClass::Variable),
            ("keyword.return", HighlightClass::Keyword),
            ("text.title", HighlightClass::Other),
        ] {
            assert_eq!(map_class(cname), class, "{cname}");
        }
        // The promoted three, at every depth they actually occur.
        for cname in [
            "string.special",
            "string.special.key",
            "string.special.symbol",
        ] {
            assert_eq!(map_class(cname), HighlightClass::StringSpecial, "{cname}");
        }
        assert_eq!(
            map_class("constant.builtin"),
            HighlightClass::ConstantBuiltin
        );
        assert_eq!(map_class("boolean"), HighlightClass::ConstantBuiltin);
        assert_eq!(map_class("constant"), HighlightClass::Constant);
        assert_eq!(
            map_class("punctuation.special"),
            HighlightClass::PunctuationSpecial
        );
    }

    // ── V76-C1 — highlight/1 snippet wire ────────────────────────────────

    fn wire(lang: Option<&str>, text: &str) -> HighlightOut {
        highlight_snippet(&HighlightIn {
            lang: lang.map(str::to_string),
            path: None,
            text: text.to_string(),
            salt: false,
        })
        .expect("fixture snippets are under the cap")
    }

    #[test]
    fn ruby_snippet_golden_line_spans() {
        insta::assert_debug_snapshot!(wire(Some("ruby"), RUBY_SNIPPET));
    }

    #[test]
    fn rust_snippet_golden_line_spans() {
        insta::assert_debug_snapshot!(wire(Some("rust"), RUST_SNIPPET));
    }

    #[test]
    fn typescript_snippet_golden_line_spans() {
        insta::assert_debug_snapshot!(wire(Some("typescript"), TYPESCRIPT_SNIPPET));
    }

    #[test]
    fn yaml_snippet_golden_line_spans() {
        insta::assert_debug_snapshot!(wire(Some("yaml"), YAML_SNIPPET));
    }

    #[test]
    fn haml_snippet_golden_line_spans() {
        insta::assert_debug_snapshot!(wire(Some("haml"), "%section#hero\n  %p= t('.title')\n"));
    }

    #[test]
    fn markdown_with_fence_golden_line_spans() {
        insta::assert_debug_snapshot!(wire(Some("markdown"), MARKDOWN_SNIPPET));
    }

    #[test]
    fn markdown_fence_guest_spans_are_in_host_line_coordinates() {
        let out = wire(Some("markdown"), MARKDOWN_SNIPPET);
        assert_eq!(out.tier, "full");
        assert!(
            out.spans
                .iter()
                .any(|s| s.line >= 5 && s.role == HighlightClass::Keyword),
            "the Ruby `class`/`end` inside the fence must paint in Markdown line numbers: {:?}",
            out.spans
        );
    }

    #[test]
    fn rb_alias_resolves_to_ruby() {
        let out = wire(Some("rb"), RUBY_SNIPPET);
        assert_eq!(out.lang, Some("ruby"));
        assert_eq!(out.tier, "full");
        assert!(!out.spans.is_empty());
    }

    #[test]
    fn path_infers_lang() {
        let out = highlight_snippet(&HighlightIn {
            lang: None,
            path: Some("lib/greet.rb".into()),
            text: RUBY_SNIPPET.into(),
            salt: true,
        })
        .unwrap();
        assert_eq!(out.lang, Some("ruby"));
        assert!(out.salt.is_some(), "salt=true must echo highlight_salt");
        assert!(out.salt.unwrap().contains("ruby"));
    }

    #[test]
    fn unknown_language_is_tier_none_never_500() {
        let out = wire(Some("cobol"), "IDENTIFICATION DIVISION.\n");
        assert_eq!(out.lang, None);
        assert_eq!(out.tier, "none");
        assert!(out.spans.is_empty());
        assert_eq!(out.honesty.derived_from, "none");
        let reason = out.honesty.reason.expect("unknown lang names why");
        assert!(reason.contains("cobol"), "{reason}");
        assert!(reason.contains("syntax/1"), "{reason}");
    }

    #[test]
    fn named_none_tier_is_honest_not_an_error() {
        let out = wire(Some("sql"), "SELECT 1;\n");
        assert_eq!(out.lang, Some("sql"));
        assert_eq!(out.tier, "none");
        assert!(out.spans.is_empty());
        assert!(out.honesty.reason.is_some());
    }

    #[test]
    fn oversize_snippet_refuses_with_the_size() {
        let n = MAX_SNIPPET_BYTES + 1;
        let err = highlight_snippet(&HighlightIn {
            lang: Some("ruby".into()),
            path: None,
            text: "x".repeat(n),
            salt: false,
        })
        .unwrap_err();
        assert_eq!(err.status_code(), axum::http::StatusCode::BAD_REQUEST);
        let msg = err.message();
        assert!(msg.contains(&n.to_string()), "{msg}");
        assert!(msg.contains(&MAX_SNIPPET_BYTES.to_string()), "{msg}");
        assert!(msg.contains("256 KiB"), "{msg}");
    }

    #[test]
    fn batch_item_cap_refuses_with_the_count() {
        let items: Vec<HighlightBatchItemIn> = (0..MAX_BATCH_ITEMS + 1)
            .map(|i| HighlightBatchItemIn {
                id: format!("i{i}"),
                lang: Some("ruby".into()),
                path: None,
                text: "x".into(),
            })
            .collect();
        let err = highlight_batch(&HighlightBatchIn { items }).unwrap_err();
        assert_eq!(err.status_code(), axum::http::StatusCode::BAD_REQUEST);
        let msg = err.message();
        assert!(msg.contains(&(MAX_BATCH_ITEMS + 1).to_string()), "{msg}");
        assert!(msg.contains(&MAX_BATCH_ITEMS.to_string()), "{msg}");
    }

    #[test]
    fn batch_byte_cap_refuses_with_the_total() {
        // Five items each AT the per-item cap: every item passes the
        // 256 KiB gate on its own, but the batch total (1.25 MiB) crosses
        // MAX_BATCH_BYTES — the refusal must name the total, the cap and
        // the human unit. (Two such items sum to 512 KiB and are ACCEPTED.)
        let chunk = "x".repeat(MAX_SNIPPET_BYTES);
        let per_item = MAX_SNIPPET_BYTES;
        let n_items = MAX_BATCH_BYTES / per_item + 1;
        let items: Vec<HighlightBatchItemIn> = (0..n_items)
            .map(|k| HighlightBatchItemIn {
                id: format!("item-{k}"),
                lang: Some("ruby".into()),
                path: None,
                text: chunk.clone(),
            })
            .collect();
        let total = per_item * n_items;
        assert!(total > MAX_BATCH_BYTES);
        let err = highlight_batch(&HighlightBatchIn { items }).unwrap_err();
        let msg = err.message();
        assert!(msg.contains(&total.to_string()), "{msg}");
        assert!(msg.contains(&MAX_BATCH_BYTES.to_string()), "{msg}");
        assert!(msg.contains("1 MiB"), "{msg}");
    }

    #[test]
    fn batch_duplicate_id_refuses_naming_the_id() {
        let err = highlight_batch(&HighlightBatchIn {
            items: vec![
                HighlightBatchItemIn {
                    id: "dup".into(),
                    lang: Some("ruby".into()),
                    path: None,
                    text: "a".into(),
                },
                HighlightBatchItemIn {
                    id: "dup".into(),
                    lang: Some("rust".into()),
                    path: None,
                    text: "b".into(),
                },
            ],
        })
        .unwrap_err();
        assert!(err.message().contains("dup"), "{}", err.message());
    }

    #[test]
    fn batch_paints_each_item() {
        let out = highlight_batch(&HighlightBatchIn {
            items: vec![
                HighlightBatchItemIn {
                    id: "rb".into(),
                    lang: Some("ruby".into()),
                    path: None,
                    text: RUBY_SNIPPET.into(),
                },
                HighlightBatchItemIn {
                    id: "unknown".into(),
                    lang: Some("cobol".into()),
                    path: None,
                    text: "x".into(),
                },
            ],
        })
        .unwrap();
        assert_eq!(out.schema, HIGHLIGHT_BATCH_SCHEMA);
        assert_eq!(out.items.len(), 2);
        assert_eq!(out.items[0].id, "rb");
        assert!(!out.items[0].spans.is_empty());
        assert_eq!(out.items[1].id, "unknown");
        assert_eq!(out.items[1].tier, "none");
        assert!(out.items[1].spans.is_empty());
    }

    #[test]
    fn line_spans_split_at_newlines_and_stay_in_bounds() {
        let src = "ab\ncd\n";
        let spans = vec![Span {
            byte_start: 1,
            byte_len: 3,
            class: HighlightClass::Comment,
        }];
        let wire = line_spans_from_bytes(src.as_bytes(), &spans);
        assert_eq!(
            wire,
            vec![
                LineRoleSpan {
                    line: 1,
                    start: 1,
                    end: 2,
                    role: HighlightClass::Comment,
                },
                LineRoleSpan {
                    line: 2,
                    start: 0,
                    end: 1,
                    role: HighlightClass::Comment,
                },
            ]
        );
    }

    #[test]
    fn v76_c1_routes_are_registered_in_router_src() {
        const ROUTER_SRC: &str = include_str!("router.rs");
        assert!(!V76_C1_ROUTES.is_empty());
        for c in V76_C1_ROUTES {
            let nested = c.path.strip_prefix("/api").expect("/api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{} missing from router.rs",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{} handler {} missing from router.rs",
                c.path,
                c.handler
            );
            assert!((c.params_accept_without)(""));
        }
    }
}
