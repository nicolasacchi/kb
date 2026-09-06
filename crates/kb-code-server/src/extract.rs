//! Symbol extraction: tree-sitter parse + a `tags.scm`-style query (either
//! the OFFICIAL bundled one, or one of `lang.rs`'s vendored/hand-written
//! ones — see that module's `tags_query` doc for which is which, across all
//! seven "tags" languages: Rust/Python/Ruby/JavaScript/TypeScript/TSX/Bash)
//! → a flat, deterministic list of DEFINITIONS only (`@reference.*`
//! captures in `tags.scm` — call sites, trait/impl references — are never
//! indexed; only `@definition.*` captures are). YAML (the eighth language)
//! is NOT a tags language at all (ADR-7) — `extract_symbols` dispatches it
//! straight to the `yaml` module's CST-walk key-path outline instead of the
//! query machinery this module doc otherwise describes.
//!
//! ## Why dedup by (start_byte, end_byte)
//!
//! `tags.scm` files declare multiple patterns that can independently match
//! the SAME node. Rust's is the clearest example:
//!
//! ```scm
//! (declaration_list
//!     (function_item name: (identifier) @name) @definition.method)
//! (function_item name: (identifier) @name) @definition.function
//! ```
//!
//! A method inside an `impl`/`trait` body is a `function_item` that is
//! ALSO a direct child of a `declaration_list` — so it matches BOTH
//! patterns, once as `@definition.method` and once (via the unrestricted
//! second pattern) as `@definition.function`. Query matching finds every
//! independent pattern match; nothing deduplicates them for us. We resolve
//! this exactly the way the upstream `tree-sitter-tags` C library does:
//! patterns are numbered in FILE declaration order (`QueryMatch::
//! pattern_index`), and for two matches that captured the identical node,
//! the one declared EARLIER in `tags.scm` wins. Since `method` is declared
//! before `function` in Rust's `tags.scm`, a method inside an impl/trait
//! body is correctly classified as `"method"`, not `"fn"`.
//!
//! ## Kind mapping
//!
//! `tags.scm` capture names (`@definition.function`/`.method`/`.class`/
//! `.interface`/`.module`/`.macro`/`.constant`) are coarser than the kind
//! vocabulary we want (Rust's `struct`/`enum`/`union`/`type_item` all share
//! `@definition.class`), so `map_kind` further disambiguates using the
//! captured node's own tree-sitter grammar `kind()`. See `map_kind` for the
//! exact table.
//!
//! ## Deliberately NOT indexed
//!
//! Rust `impl` blocks and `const` items are not their own `symbols` rows:
//! the official `tags.scm` doesn't tag either as a `@definition.*` (an impl
//! block reference is `@reference.implementation` only — ctags-style tools
//! don't index impl blocks as symbols either, since `impl Trait for Type`
//! has no single "name"). Methods inside an `impl` block ARE indexed
//! (kind `"method"`), with `container` set to the impl's target type name
//! via an AST walk-up (see `container_of`) — so the impl block's identity
//! is still recoverable, just not as its own row.
//!
//! `signature`/`doc` are POPULATED (B2) for the four `lang::
//! TOKEN_LEVEL_LANG_IDS` languages (Rust/TypeScript/TSX/JavaScript) — see
//! [`build_signature`]/[`capture_doc`] below — and stay `None` for every
//! other language (Python/Ruby/Bash/Go/YAML/TOML/JSON), unchanged from
//! before B2.
//!
//! ## Signature capture (B2)
//!
//! [`build_signature`] slices the symbol node's own source text from its
//! start to the start of its "body" field (a per-language, per-node-kind
//! table, [`body_field_name`]) — e.g. `fn add(a: i32, b: i32) -> i32 ` for a
//! Rust `function_item` (the trailing `{` and everything after it is the
//! body, excluded). A node with NO body field entry (a bodiless trait
//! method, a `type X = Y;` alias, a macro definition, or an arrow-function/
//! function-expression bound to a `const` — whose captured node is the
//! `variable_declarator`, not the function value itself, so there is no
//! single "body" to cut before) falls back to the FULL node text, still
//! capped. Either way the result collapses internal whitespace runs to a
//! single space and is capped at [`SIGNATURE_CAP`] chars.
//!
//! ## Doc-comment capture (B2)
//!
//! [`capture_doc`] walks BACKWARD through the symbol node's preceding
//! siblings (comments are `extra` grammar rules — tree-sitter still emits
//! them as ordinary sibling nodes, just not required by any rule — see
//! `tree-sitter-rust`'s own `node-types.json`), collecting a CONTIGUOUS run
//! (each comment ending exactly one row before the next thing starts — a
//! blank line anywhere breaks the run) of doc-worthy comments: Rust accepts
//! only `///`-prefixed line comments and `/** */`-prefixed block comments
//! (a plain `//`/`/* */` non-doc comment, or `//!`/`/*!` inner-doc comments,
//! stop the walk — Rust has an explicit doc-comment marker, so this pass
//! honors it); TypeScript/TSX/JavaScript accept ANY `comment` node (JSDoc
//! blocks AND plain `//` runs both count — JS/TS has no dedicated doc
//! marker, so both conventions are treated as documentation). Collected
//! lines are reversed back to source order, joined, comment markers/leading
//! `*` stripped, whitespace-collapsed, and capped at [`DOC_CAP`] chars.

use crate::lang::{self, LangError};
use std::collections::BTreeMap;
use tree_sitter::StreamingIterator;

pub type Result<T> = std::result::Result<T, LangError>;

/// [`build_signature`]'s output cap, in chars (post whitespace-collapse).
pub const SIGNATURE_CAP: usize = 200;
/// [`capture_doc`]'s output cap, in chars (post whitespace-collapse).
pub const DOC_CAP: usize = 400;
/// [`extract_todos`]'s trailing-text cap, in chars (post trim).
pub const TODO_TEXT_CAP: usize = 200;

/// Markers scanned in comment text (word-boundary, case-sensitive). Order
/// is longest-first so a hypothetical future multi-char overlap prefers
/// the longer form; today every marker is unique.
pub const TODO_MARKERS: &[&str] = &["FIXME", "TODO", "HACK", "XXX", "BUG"];

/// One TODO-style marker hit from a comment. `line` is 1-based (same
/// convention as [`Symbol::line_start`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoHit {
    pub line: u32,
    pub marker: String,
    /// Trailing text after the marker, trimmed, capped at [`TODO_TEXT_CAP`].
    pub text: String,
}

/// One indexed definition. `line_start`/`line_end` are 1-based (tree-sitter
/// rows are 0-based; `+1` here matches editor/human display convention).
/// `col_start`/`col_end` are 0-based byte offsets within their line
/// (tree-sitter's own convention, LSP-compatible).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Symbol {
    /// Stable emission order (byte position ascending) — see the module
    /// doc; exists to make `(blob_hash, salt, ordinal)` a unique key, not
    /// meaningful on its own.
    pub ordinal: u32,
    pub name: String,
    pub kind: String,
    pub line_start: u32,
    pub line_end: u32,
    pub col_start: u32,
    pub col_end: u32,
    /// The nearest enclosing named construct (impl target type / trait /
    /// mod / class / module — see `container_of`), if any.
    pub container: Option<String>,
    /// `Some` for the four `lang::TOKEN_LEVEL_LANG_IDS` languages (B2 —
    /// see [`build_signature`]); `None` for every other language.
    pub signature: Option<String>,
    /// `Some` for the four `lang::TOKEN_LEVEL_LANG_IDS` languages when a
    /// doc comment directly precedes the symbol (B2 — see
    /// [`capture_doc`]); `None` otherwise (no grammar coverage, or no doc
    /// comment found).
    pub doc: Option<String>,
    /// V3.G2 — minimum positional arity this symbol accepts (from
    /// signature parse). `None` = unknown. See `crate::intel::arity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_min: Option<u32>,
    /// V3.G2 — maximum positional arity (`None` when unknown OR unbounded
    /// via varargs/`...`/`*args`). Distinct from min: `(Some(1), None)`
    /// means "at least 1, no upper bound".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_max: Option<u32>,
}

/// A definition-capture candidate before kind resolution, tracked per
/// `(start_byte, end_byte)` so we can keep only the earliest-declared
/// pattern's classification for a node matched by more than one pattern.
/// `suffix` is owned (not `&str`): `Query::capture_names()` ties its
/// borrow to the `Query` value's own lifetime (elision collapses the
/// outer-slice and inner-`&str` lifetimes to the same one), not `'static`
/// — even though the underlying data IS `'static` in memory, the API
/// doesn't expose that, so we copy the (short) suffix out rather than
/// fight the borrow.
#[derive(Clone)]
struct Candidate<'tree> {
    pattern_index: usize,
    suffix: String,
    node: tree_sitter::Node<'tree>,
    name: String,
}

/// Scan comment nodes for `TODO`/`FIXME`/`HACK`/`XXX`/`BUG` (word-boundary,
/// case-sensitive) and capture the trailing text to end-of-line (trimmed,
/// capped at [`TODO_TEXT_CAP`]).
///
/// Gated to the eight full-tier / token-level languages
/// (`lang::supports_token_level`): outline-tier files (yaml/toml/json) are
/// skipped in v3 scope — callers still get an empty `Vec`, never an error.
/// Comment-node detection reuses the same kind vocabulary `is_doc_comment`
/// already knows (`line_comment`/`block_comment`/`comment`), walked via a
/// full-tree visitor rather than the highlights query (cheaper than a
/// second query compile, and correct for every grammar that tags comments
/// as named nodes — which all eight full-tier languages do).
///
/// Scanning comment nodes (not raw lines) means a decoy `"TODO"` inside a
/// string literal does NOT match — pinned by the unit tests below.
pub fn extract_todos(lang_id: &str, source: &[u8]) -> Result<Vec<TodoHit>> {
    if !lang::supports_token_level(lang_id) {
        return Ok(Vec::new());
    }
    let (tree, _) = lang::parse(lang_id, source)?;
    let mut out = Vec::new();
    walk_comments_for_todos(tree.root_node(), source, &mut out);
    Ok(out)
}

fn is_comment_kind(kind: &str) -> bool {
    // Rust: line_comment / block_comment. TS/JS/Go/Ruby/Bash/Python: comment.
    // Defensive `contains("comment")` covers a future grammar rename without
    // silently dropping the pass.
    matches!(kind, "comment" | "line_comment" | "block_comment") || kind.contains("comment")
}

fn walk_comments_for_todos(node: tree_sitter::Node<'_>, source: &[u8], out: &mut Vec<TodoHit>) {
    if is_comment_kind(node.kind()) {
        scan_comment_for_todos(node, source, out);
        return; // never walk into a comment's children
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_comments_for_todos(child, source, out);
    }
}

fn scan_comment_for_todos(node: tree_sitter::Node<'_>, source: &[u8], out: &mut Vec<TodoHit>) {
    let Ok(text) = node.utf8_text(source) else {
        return;
    };
    let base_row = node.start_position().row;
    for (i, line) in text.lines().enumerate() {
        if let Some((marker, rest)) = find_todo_marker(line) {
            let text = cap_chars(rest.trim(), TODO_TEXT_CAP);
            out.push(TodoHit {
                line: (base_row + i + 1) as u32,
                marker: marker.to_string(),
                text,
            });
        }
    }
}

/// First word-boundary match of any [`TODO_MARKERS`] entry on `line`.
/// Returns `(marker, trailing_text_after_marker)`.
fn find_todo_marker(line: &str) -> Option<(&'static str, &str)> {
    let bytes = line.as_bytes();
    for &marker in TODO_MARKERS {
        let mut start = 0;
        while start + marker.len() <= line.len() {
            if let Some(rel) = line[start..].find(marker) {
                let abs = start + rel;
                let before_ok = abs == 0 || !is_word_byte(bytes[abs - 1]);
                let after = abs + marker.len();
                let after_ok = after >= bytes.len() || !is_word_byte(bytes[after]);
                if before_ok && after_ok {
                    return Some((marker, &line[after..]));
                }
                start = abs + 1;
            } else {
                break;
            }
        }
    }
    None
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The languages whose symbols come from a direct CST WALK (a hierarchical
/// key-path outline) rather than a `tags.scm` query — see the dispatch at
/// the top of [`extract_symbols`] and `crate::yaml`/`crate::keypath`'s
/// module docs. Named as a const (V72-H1) because the `syntax/1` Parity
/// Grid has to tell "symbols, as code definitions" from "symbols, as key
/// paths" without restating this list; the dispatch below is still the
/// implementation and `extract_symbols_dispatch_matches_the_cst_outline_set`
/// pins the two together.
pub const CST_OUTLINE_LANG_IDS: &[&str] = &["yaml", "toml", "json"];

pub fn extract_symbols(lang_id: &str, source: &[u8]) -> Result<Vec<Symbol>> {
    // YAML has no tags.scm at all (ADR-7) — a hierarchical key-path outline
    // via a direct CST walk instead; see `crate::yaml`'s module doc. TOML
    // and JSON (W2.6) ride the same model via `crate::keypath`.
    if lang_id == "yaml" {
        return Ok(crate::yaml::outline(source)?.symbols);
    }
    if lang_id == "toml" {
        return Ok(crate::keypath::outline_toml(source)?.symbols);
    }
    if lang_id == "json" {
        return Ok(crate::keypath::outline_json(source)?.symbols);
    }
    // PRR-N3 — ERB has no `tags.scm` (deliberately not one of
    // `lang::TOKEN_LEVEL_LANG_IDS`; see `lang::ERB`'s doc). Without this
    // short-circuit, `lang::tags_query("erb")` misses and this fn would
    // return `LangError::Unsupported`, which `ingest::index_file` propagates
    // straight out of the per-file walk (`?`) — aborting the ENTIRE repo
    // walk on the first `.erb` file. Empty symbols, same as any other
    // non-tags language reaching this point.
    if lang_id == "erb" {
        return Ok(Vec::new());
    }
    // V72-H3 (D7) — HAML has no tree-sitter grammar at all; its rows come
    // from this crate's OWN scanner (`crate::haml`, `syntax/1`'s
    // `Engine::Scanner`). Structurally the same dispatch as the three CST
    // outline languages above, one line earlier than `lang::parse` would
    // fail with `Unsupported`.
    if lang_id == "haml" {
        return Ok(crate::haml::outline(source));
    }
    let (tree, language) = lang::parse(lang_id, source)?;
    let tags_src =
        lang::tags_query(lang_id).ok_or_else(|| LangError::Unsupported(lang_id.to_string()))?;
    let query = lang::compile_query(lang_id, &language, tags_src)?;
    let capture_names = query.capture_names();

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source);

    // BTreeMap keyed on (start_byte, end_byte) — iteration order is
    // already byte-position ascending, which doubles as our deterministic
    // emission order (no separate sort needed).
    let mut best: BTreeMap<(usize, usize), Candidate<'_>> = BTreeMap::new();

    while let Some(m) = matches.next() {
        let mut def: Option<(String, tree_sitter::Node<'_>)> = None;
        let mut name: Option<String> = None;
        for cap in m.captures {
            let cname = capture_names[cap.index as usize];
            if let Some(suffix) = cname.strip_prefix("definition.") {
                def = Some((suffix.to_string(), cap.node));
            } else if cname == "name" {
                name = Some(cap.node.utf8_text(source).unwrap_or("").to_string());
            }
        }
        let (Some((suffix, node)), Some(name)) = (def, name) else {
            continue;
        };
        let key = (node.start_byte(), node.end_byte());
        let candidate = Candidate {
            pattern_index: m.pattern_index,
            suffix,
            node,
            name,
        };
        best.entry(key)
            .and_modify(|existing| {
                if candidate.pattern_index < existing.pattern_index {
                    *existing = candidate.clone();
                }
            })
            .or_insert(candidate);
    }

    let mut symbols = Vec::with_capacity(best.len());
    for (ordinal, (_, c)) in best.into_iter().enumerate() {
        let Some(kind) = map_kind(lang_id, &c.suffix, c.node) else {
            continue;
        };
        let start = c.node.start_position();
        let end = c.node.end_position();
        // B2 — signature/doc are only derived for the token-level languages
        // (see the module doc); every other language keeps the pre-B2
        // `None`/`None` behavior unchanged.
        let (signature, doc) = if lang::supports_token_level(lang_id) {
            (
                Some(build_signature(lang_id, c.node, source)),
                capture_doc(lang_id, c.node, source),
            )
        } else {
            (None, None)
        };
        // V3.G2 — parse arity from the cut signature when present. Pure
        // string parse (no second AST walk); fails open to None/None.
        let (param_min, param_max) = signature
            .as_deref()
            .map(crate::intel::arity::param_range_from_signature)
            .unwrap_or((None, None));
        symbols.push(Symbol {
            ordinal: ordinal as u32,
            name: c.name,
            kind: kind.to_string(),
            line_start: start.row as u32 + 1,
            line_end: end.row as u32 + 1,
            col_start: start.column as u32,
            col_end: end.column as u32,
            container: container_of(lang_id, c.node, source),
            signature,
            doc,
            param_min,
            param_max,
        });
    }
    Ok(symbols)
}

/// Per-language, per-node-kind "body" field name — the field whose child
/// marks where a signature should be cut (see the module doc). Verified
/// against each grammar crate's own `node-types.json`, not guessed: every
/// entry here genuinely has a field named `"body"` holding the
/// block/class-body/declaration-list. A node kind with NO entry (a bodiless
/// item, or a `variable_declarator` — the node the TS/JS arrow-const
/// pattern in `queries/typescript-tags.scm` actually captures, which has a
/// `value` field, not a `body` one) falls back to the full-node path in
/// [`build_signature`].
fn body_field_name(lang_id: &str, node_kind: &str) -> Option<&'static str> {
    match (lang_id, node_kind) {
        (
            "rust",
            "function_item" | "struct_item" | "enum_item" | "union_item" | "trait_item"
            | "mod_item",
        ) => Some("body"),
        (
            "typescript" | "tsx" | "javascript",
            "function_declaration"
            | "generator_function_declaration"
            | "method_definition"
            | "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "internal_module",
        ) => Some("body"),
        // Python (B5b) — both cut at the `:` (the body field's first byte
        // sits right after it), e.g. `def add(a, b) -> int:`.
        ("python", "function_definition" | "class_definition") => Some("body"),
        // Ruby (B5b) — `method`/`singleton_method`'s `body` field type
        // includes `_arg`/`rescue_modifier` too (an endless `def f = expr`
        // form), but the common bodied case is `body_statement`; either way
        // the cut point is the SAME field.
        ("ruby", "method" | "singleton_method" | "class" | "module") => Some("body"),
        // Go (B5b) — `type_spec`/`type_alias`/`const_spec`/`var_spec`/
        // `field_declaration` have NO `body` field (verified against
        // `tree-sitter-go 0.25.0`'s `node-types.json`) — those fall back to
        // the full-node path below, same as Rust's bodiless items.
        ("go", "function_declaration" | "method_declaration") => Some("body"),
        // Bash (B5b) — cuts at `{`/`function name ` either surface form.
        ("bash", "function_definition") => Some("body"),
        _ => None,
    }
}

/// Slice `node`'s own source text from its start to the start of its body
/// field (or the full node when [`body_field_name`] has no entry for its
/// kind — bodiless items and arrow/function-expression consts, see that
/// fn's doc), collapse whitespace runs, cap at [`SIGNATURE_CAP`] chars. See
/// the module doc's "Signature capture" section.
fn build_signature(lang_id: &str, node: tree_sitter::Node<'_>, source: &[u8]) -> String {
    let end_byte = body_field_name(lang_id, node.kind())
        .and_then(|field| node.child_by_field_name(field))
        .map(|body| body.start_byte())
        .unwrap_or_else(|| node.end_byte())
        .max(node.start_byte());
    let slice = std::str::from_utf8(&source[node.start_byte()..end_byte]).unwrap_or("");
    cap_chars(&collapse_whitespace(slice), SIGNATURE_CAP)
}

/// `true` for a comment node this language treats as documentation — see
/// the module doc's "Doc-comment capture" section for the Rust-vs-TS/JS
/// asymmetry. Ruby/Go/Bash (B5b) all follow the TS/JS rule (no dedicated
/// doc-marker syntax of their own — RDoc/YARD, godoc, and Bash's own
/// convention all just read a plain preceding `#`/`//` comment run as
/// documentation) — Python is NOT in this table at all: its docstring
/// convention is structurally different (a string literal INSIDE the body,
/// not a preceding comment), handled entirely by [`python_docstring`]
/// instead, called from a separate branch in [`capture_doc`] before this fn
/// is ever consulted for `"python"`.
fn is_doc_comment(lang_id: &str, node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let Ok(text) = node.utf8_text(source) else {
        return false;
    };
    match lang_id {
        "rust" => {
            (node.kind() == "line_comment" && text.starts_with("///") && !text.starts_with("////"))
                || (node.kind() == "block_comment"
                    && text.starts_with("/**")
                    && !text.starts_with("/***"))
        }
        "typescript" | "tsx" | "javascript" | "ruby" | "go" | "bash" => node.kind() == "comment",
        _ => false,
    }
}

/// Python's docstring: a STRING LITERAL as the very FIRST statement inside a
/// `function_definition`/`class_definition`'s `body` block — the language's
/// actual documentation convention (`help()`/Sphinx/etc. all read this, not
/// a preceding comment), structurally unlike every other language
/// [`capture_doc`] handles. Concatenates the `string` node's own children
/// EXCLUDING the quote delimiters (`string_start`/`string_end`) — verbatim,
/// escape sequences and all (same "captured as literal source text, never
/// interpreted" posture as [`strip_comment_markers`]'s comment-marker
/// stripping) — collapses whitespace, caps at [`DOC_CAP`]. `None` for
/// anything that isn't exactly this shape: no `body` field (a plain
/// assignment/parameter — nothing to look inside), an empty body, a first
/// statement that isn't a bare string expression, or an empty/whitespace-only
/// string.
fn python_docstring(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let first = body.named_child(0)?;
    if first.kind() != "expression_statement" {
        return None;
    }
    let expr = first.named_child(0)?;
    if expr.kind() != "string" {
        return None;
    }
    let mut raw = String::new();
    let mut cursor = expr.walk();
    for child in expr.named_children(&mut cursor) {
        if matches!(child.kind(), "string_start" | "string_end") {
            continue;
        }
        if let Ok(text) = child.utf8_text(source) {
            raw.push_str(text);
        }
    }
    if raw.trim().is_empty() {
        return None;
    }
    Some(cap_chars(&collapse_whitespace(&raw), DOC_CAP))
}

/// Walk `node`'s preceding siblings collecting a CONTIGUOUS run of doc
/// comments (see [`is_contiguous`] for exactly what "contiguous" means),
/// stop at the first non-doc-comment sibling, reverse back to source order,
/// strip comment markers, collapse whitespace, cap at [`DOC_CAP`] chars.
/// `None` when no doc comment directly precedes `node`. Python is handled
/// entirely by [`python_docstring`] instead (see that fn's doc) — this
/// preceding-comment-run model doesn't apply to it at all.
fn capture_doc(lang_id: &str, node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if lang_id == "python" {
        return python_docstring(node, source);
    }
    let mut lines: Vec<String> = Vec::new();
    let mut next_start_byte = node.start_byte();
    let mut cur = node.prev_sibling();
    while let Some(prev) = cur {
        if !is_doc_comment(lang_id, prev, source) {
            break;
        }
        if !is_contiguous(prev, next_start_byte, source) {
            break; // a blank line (or anything else) separates it — stop
        }
        let text = prev.utf8_text(source).unwrap_or("");
        lines.push(strip_comment_markers(text));
        next_start_byte = prev.start_byte();
        cur = prev.prev_sibling();
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    Some(cap_chars(&collapse_whitespace(&lines.join(" ")), DOC_CAP))
}

/// `true` when nothing but a SINGLE line break (and possibly other
/// whitespace on that one line) separates `prev`'s own content from
/// whatever starts at `next_start_byte` — i.e. no BLANK line in between.
/// Byte-based, not row-based, and deliberately NOT keyed on `prev`'s node
/// kind: some comment tokens consume their own trailing newline as part of
/// the node's span (verified empirically — Rust's `line_comment` does;
/// `block_comment` doesn't), so comparing `Point::row`s directly is off by
/// one for exactly HALF of the comment kinds this module cares about. Byte
/// distance from `prev`'s own trimmed-of-trailing-newline text isn't
/// ambiguous either way: strip any newline `prev`'s OWN text already ends
/// with, then count newlines in the gap up to `next_start_byte` — zero (same
/// line) or one (an ordinary line break) is contiguous; two or more is a
/// blank line.
fn is_contiguous(prev: tree_sitter::Node<'_>, next_start_byte: usize, source: &[u8]) -> bool {
    let prev_text = prev.utf8_text(source).unwrap_or("");
    let trimmed_len = prev_text.trim_end_matches(['\n', '\r']).len();
    let effective_end_byte = prev.start_byte() + trimmed_len;
    if effective_end_byte > next_start_byte {
        return false; // defensive; siblings should never overlap
    }
    let gap = &source[effective_end_byte..next_start_byte];
    gap.iter().filter(|&&b| b == b'\n').count() <= 1
}

/// Strip a single comment node's OWN markers — `///`/`//!`/`//` line
/// prefixes, or a `/**`/`/*!`/`/*` ... `*/` block wrapper (with each
/// interior line's leading `*` also stripped, the common doc-block
/// convention: `/**\n * text\n */`) — leaving just the prose.
fn strip_comment_markers(raw: &str) -> String {
    let trimmed = raw.trim();
    for prefix in ["/**", "/*!", "/*"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let body = rest.strip_suffix("*/").unwrap_or(rest);
            return body
                .lines()
                .map(|l| {
                    let l = l.trim();
                    l.strip_prefix('*').map(|s| s.trim()).unwrap_or(l)
                })
                .filter(|l| !l.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    for prefix in ["///", "//!", "//"] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    // Ruby/Bash (B5b) — the `#` line-comment marker. Unambiguous to add
    // unconditionally (no OTHER language this fn ever sees uses a bare `#`
    // as its own comment marker), so this isn't gated per-language like the
    // C-style prefixes above.
    if let Some(rest) = trimmed.strip_prefix('#') {
        return rest.trim().to_string();
    }
    trimmed.to_string()
}

/// Collapse every run of whitespace (spaces, tabs, newlines) to a single
/// space, trimming the ends — shared by [`build_signature`]/[`capture_doc`]
/// so a multi-line node/comment always serializes as one tidy line.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(ch);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

/// Truncate `s` to at most `max` CHARS (not bytes — safe on multibyte
/// UTF-8 input, unlike a raw byte-slice truncation).
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Map a `tags.scm` `@definition.<suffix>` capture, disambiguated by the
/// captured node's own grammar `kind()`, to our fixed symbol-kind
/// vocabulary. `None` means "not indexed" (defensive default for a
/// `tags.scm` capture this table doesn't recognize yet — forward-
/// compatible with a future grammar/query bump rather than a hard error).
///
/// | lang   | capture suffix | node kind          | our `kind`         |
/// |--------|-----------------|---------------------|---------------------|
/// | rust   | function        | function_item        | `fn`                |
/// | rust   | method          | function_item         | `method`            |
/// | rust   | class            | struct_item           | `struct`            |
/// | rust   | class            | enum_item              | `enum`              |
/// | rust   | class            | union_item             | `union`             |
/// | rust   | class            | type_item               | `type_alias`        |
/// | rust   | interface        | trait_item             | `trait`             |
/// | rust   | module           | mod_item               | `mod`               |
/// | rust   | macro            | macro_definition       | `macro`             |
/// | python | class            | class_definition       | `class`             |
/// | python | function         | function_definition    | `def`               |
/// | python | constant         | assignment              | `const`             |
/// | ruby   | method           | method                    | `method`            |
/// | ruby   | method           | singleton_method        | `singleton_method` |
/// | ruby   | method           | alias                     | `alias`             |
/// | ruby   | class            | class                     | `class`             |
/// | ruby   | class            | singleton_class          | `singleton_class`  |
/// | ruby   | module           | module                    | `module`            |
/// | typescript/tsx | function | (any)                     | `function`          |
/// | typescript/tsx | method   | (any)                     | `method`            |
/// | typescript/tsx | class    | (any)                     | `class`             |
/// | typescript/tsx | interface | (any)                    | `interface`         |
/// | typescript/tsx | type_alias | (any)                   | `type_alias`        |
/// | typescript/tsx | enum     | (any)                     | `enum`              |
/// | typescript/tsx | module   | (any)                     | `module`            |
/// | typescript/tsx | constant | (any)                     | `const`             |
/// | javascript     | function | (any)                     | `function`          |
/// | javascript     | method   | (any)                     | `method`            |
/// | javascript     | class    | (any)                     | `class`             |
/// | javascript     | constant | (any)                     | `const`             |
/// | bash           | function | function_definition       | `function`          |
/// | go             | function | function_declaration      | `func`              |
/// | go             | method   | method_declaration         | `method`            |
/// | go             | type     | type_spec/type_alias (type: struct_type)    | `struct`    |
/// | go             | type     | type_spec/type_alias (type: interface_type) | `interface` |
/// | go             | type     | type_spec/type_alias (other)                | `type`      |
/// | go             | constant | const_spec                 | `const`             |
/// | go             | variable | var_spec                   | `var`               |
///
/// TypeScript/TSX/JavaScript/Bash don't need `node_kind` disambiguation the
/// way Rust/Ruby do — each capture suffix in kb-code's vendored/official
/// queries for those four languages already maps 1:1 onto one kind (the
/// disambiguation Rust needs happens because ONE suffix, `@definition.class`,
/// covers four distinct Rust node kinds; TS/JS/Bash's queries simply don't
/// share a suffix across unrelated node kinds). Go's `type` suffix needs the
/// SAME disambiguation Rust's `class` suffix does — see `go-tags.scm`'s
/// header: the `type_spec`/`type_alias` captures both cover plain aliases,
/// struct types, AND interface types alike, so `map_kind` inspects the
/// captured node's own `type:` field (`child_by_field_name("type")`) rather
/// than `node_kind` directly (`node_kind` here is `"type_spec"` OR
/// `"type_alias"` depending on which Go syntax form matched — the
/// disambiguator is one level DEEPER than for Rust/Ruby either way, hence
/// the separate `go_type_kind` helper below rather than a `match node_kind`
/// arm).
fn map_kind(lang_id: &str, suffix: &str, node: tree_sitter::Node<'_>) -> Option<&'static str> {
    let node_kind = node.kind();
    match (lang_id, suffix) {
        ("rust", "function") => Some("fn"),
        ("rust", "method") => Some("method"),
        ("rust", "class") => match node_kind {
            "struct_item" => Some("struct"),
            "enum_item" => Some("enum"),
            "union_item" => Some("union"),
            "type_item" => Some("type_alias"),
            _ => None,
        },
        ("rust", "interface") => Some("trait"),
        ("rust", "module") => Some("mod"),
        ("rust", "macro") => Some("macro"),

        ("python", "class") => Some("class"),
        ("python", "function") => Some("def"),
        ("python", "constant") => Some("const"),

        ("ruby", "method") => match node_kind {
            "singleton_method" => Some("singleton_method"),
            "alias" => Some("alias"),
            _ => Some("method"),
        },
        ("ruby", "class") => match node_kind {
            "singleton_class" => Some("singleton_class"),
            _ => Some("class"),
        },
        ("ruby", "module") => Some("module"),

        ("typescript" | "tsx", "function") => Some("function"),
        ("typescript" | "tsx", "method") => Some("method"),
        ("typescript" | "tsx", "class") => Some("class"),
        ("typescript" | "tsx", "interface") => Some("interface"),
        ("typescript" | "tsx", "type_alias") => Some("type_alias"),
        ("typescript" | "tsx", "enum") => Some("enum"),
        ("typescript" | "tsx", "module") => Some("module"),
        ("typescript" | "tsx", "constant") => Some("const"),

        ("javascript", "function") => Some("function"),
        ("javascript", "method") => Some("method"),
        ("javascript", "class") => Some("class"),
        ("javascript", "constant") => Some("const"),

        ("bash", "function") => Some("function"),

        ("go", "function") => Some("func"),
        ("go", "method") => Some("method"),
        ("go", "type") => Some(go_type_kind(node)),
        ("go", "constant") => Some("const"),
        ("go", "variable") => Some("var"),

        _ => None,
    }
}

/// Disambiguate a Go `@definition.type` capture (a `type_spec` OR
/// `type_alias` node — see `map_kind`'s doc table on why Go's `= ` alias
/// syntax is a distinct grammar node) by inspecting its OWN `type:` field
/// (both node kinds carry one): `struct_type`/`interface_type` classify as
/// `"struct"`/`"interface"`; anything else (a named-type reference,
/// pointer/slice/map/function type, ...) is a plain `"type"`.
fn go_type_kind(type_spec: tree_sitter::Node<'_>) -> &'static str {
    match type_spec
        .child_by_field_name("type")
        .map(|n| n.kind())
        .unwrap_or_default()
    {
        "struct_type" => "struct",
        "interface_type" => "interface",
        _ => "type",
    }
}

/// `false` for a symbol kind that is NOT a real code symbol and should be
/// excluded from a future "repo map" (a compact per-file symbol skeleton
/// for agent context) — currently just YAML's `"key"` outline rows (see
/// `crate::yaml`'s module doc). A KIND-based filter, not a new `symbols`
/// column: every row already carries `kind`, so this needs no migration and
/// can never drift out of sync with a hand-constructed row the way a
/// separate boolean flag could.
/// Symbol kinds that describe a file's SHAPE rather than a definition
/// anything can call or import: YAML/TOML/JSON's key-path rows and V72-H3's
/// HAML template rows. Every one of them is a real `symbols` row (the
/// structure popup renders them) and none of them belongs in a repo map,
/// which is a map of DEFINITIONS.
pub const OUTLINE_ONLY_KINDS: &[&str] = &[
    "key",
    crate::haml::extract::KIND_ELEMENT,
    crate::haml::extract::KIND_FILTER,
];

pub fn is_repo_map_symbol(kind: &str) -> bool {
    !OUTLINE_ONLY_KINDS.contains(&kind)
}

/// Per-language "container-worthy" ancestor kinds: `(node_kind,
/// name_field)`. Walks `node`'s parent chain and returns the name-field
/// text of the NEAREST matching ancestor — e.g. a method's container is
/// the enclosing `impl`'s target type (Rust) or the enclosing class
/// (Python/Ruby). Returns `None` at the top level (no matching ancestor).
///
/// Go is a special case, handled entirely by [`go_container_of`] instead of
/// this ancestor-walk table: a Go method has no enclosing `impl`/`class`
/// body at all (methods are TOP-LEVEL declarations; the receiver type name
/// lives on the `method_declaration` node's OWN `receiver` field, not on any
/// ancestor), so the generic "walk up parents looking for a table match"
/// shape this function otherwise implements doesn't apply.
fn container_of(lang_id: &str, node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if lang_id == "go" {
        return go_container_of(node, source);
    }
    let table: &[(&str, &str)] = match lang_id {
        "rust" => &[
            ("impl_item", "type"),
            ("trait_item", "name"),
            ("mod_item", "name"),
        ],
        "python" => &[
            ("class_definition", "name"),
            ("function_definition", "name"),
        ],
        "ruby" => &[
            ("class", "name"),
            ("module", "name"),
            ("singleton_class", "value"),
            ("method", "name"),
            ("singleton_method", "name"),
        ],
        // TypeScript/TSX: a method's container is its enclosing class/
        // interface; a nested declaration's container is its enclosing
        // namespace. Deliberately excludes `variable_declarator` (the
        // arrow-const symbol node) — see extract.rs's W2.2 design note in
        // the crate history for why: it would make "the nearest enclosing
        // const binding" a container for code nested inside ANY const
        // value (not just function-valued ones), which is more confusing
        // than helpful for a plain data literal.
        "typescript" | "tsx" => &[
            ("class_declaration", "name"),
            ("abstract_class_declaration", "name"),
            ("interface_declaration", "name"),
            ("internal_module", "name"),
        ],
        // JavaScript: both the statement (`class Foo {}`) and expression
        // (`const Foo = class {}`) forms — the latter has no `name` field
        // on unnamed class expressions, so `child_by_field_name` simply
        // returns `None` for those (no panic, no container).
        "javascript" => &[("class_declaration", "name"), ("class", "name")],
        _ => &[],
    };
    let mut cur = node.parent();
    while let Some(n) = cur {
        if let Some((_, field)) = table.iter().copied().find(|(kind, _)| *kind == n.kind()) {
            if let Some(name_node) = n.child_by_field_name(field) {
                if let Ok(text) = name_node.utf8_text(source) {
                    return Some(text.to_string());
                }
            }
        }
        cur = n.parent();
    }
    None
}

/// Go's method-on-receiver container: `method_declaration`'s `receiver`
/// field (`(p Point)` or `(p *Point)`) names the target type directly — see
/// `container_of`'s doc for why this bypasses the generic ancestor-walk
/// table entirely. `receiver` is a `parameter_list` with exactly one
/// `parameter_declaration`, whose `type` field is either a bare
/// `type_identifier` (value receiver) or a `pointer_type` wrapping one
/// (pointer receiver) — both resolve to the same container name.
fn go_container_of(node: tree_sitter::Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "method_declaration" {
        return None;
    }
    let receiver = node.child_by_field_name("receiver")?;
    let mut cursor = receiver.walk();
    let param = receiver
        .named_children(&mut cursor)
        .find(|n| n.kind() == "parameter_declaration")?;
    let mut type_node = param.child_by_field_name("type")?;
    if type_node.kind() == "pointer_type" {
        type_node = type_node.named_child(0)?;
    }
    type_node.utf8_text(source).ok().map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// V72-H1 — the const and the dispatch must not drift: every id in
    /// [`CST_OUTLINE_LANG_IDS`] must reach a CST-walk arm (proven by
    /// producing symbols with NO tags query available), and no id outside
    /// it may.
    #[test]
    fn extract_symbols_dispatch_matches_the_cst_outline_set() {
        for id in CST_OUTLINE_LANG_IDS {
            assert!(
                lang::tags_query(id).is_none(),
                "{id}: a CST-outline language must have no tags query"
            );
            // A one-key document in each of the three formats — enough to
            // prove the walk ran rather than the query path.
            let src: &[u8] = match *id {
                "yaml" => b"a: 1\n",
                "toml" => b"a = 1\n",
                "json" => b"{\"a\": 1}",
                other => panic!("no fixture for {other}"),
            };
            let symbols = extract_symbols(id, src).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(!symbols.is_empty(), "{id}: CST outline produced no rows");
            assert!(
                symbols.iter().all(|s| s.kind == "key"),
                "{id}: a CST outline mints key rows"
            );
        }
        for id in lang::TAGS_LANG_IDS {
            assert!(
                !CST_OUTLINE_LANG_IDS.contains(id),
                "{id}: a tags language must not also be a CST-outline language"
            );
        }
    }

    fn names(symbols: &[Symbol]) -> Vec<(&str, &str, Option<&str>)> {
        symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind.as_str(), s.container.as_deref()))
            .collect()
    }

    const RUST_FIXTURE: &str = r#"
struct Point {
    x: i32,
    y: i32,
}

enum Shape {
    Circle,
    Square,
}

// A trait method with no body is a `function_signature_item` in the Rust
// grammar, NOT a `function_item` — the official tags.scm's method/function
// patterns only target `function_item`, so this line deliberately
// produces ZERO symbols (proven by its absence from the expected list
// below), unlike the two default-bodied methods on `impl Area for Point`.
trait Area {
    fn area(&self) -> f64;
}

impl Point {
    fn origin() -> Point {
        Point { x: 0, y: 0 }
    }

    fn dist(&self, other: &Point) -> f64 {
        0.0
    }
}

impl Area for Point {
    fn area(&self) -> f64 {
        1.0
    }
}

fn top_level(a: i32) -> i32 {
    a
}

// `fn helper` sits directly inside `mod util`'s `declaration_list` body —
// the SAME node type an impl/trait body uses — so the official tags.scm's
// unrestricted "function_item inside any declaration_list" pattern tags it
// `@definition.method` too, not `@definition.function`. This is an
// upstream tags.scm characteristic (a nested-mod function classifies as
// "method", not "fn"), not a kb-code bug — deliberately asserted below
// rather than special-cased away.
mod util {
    fn helper() -> i32 {
        1
    }
}
"#;

    #[test]
    fn rust_golden_symbol_list() {
        let symbols = extract_symbols("rust", RUST_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            names(&symbols),
            vec![
                ("Point", "struct", None),
                ("Shape", "enum", None),
                ("Area", "trait", None),
                ("origin", "method", Some("Point")),
                ("dist", "method", Some("Point")),
                ("area", "method", Some("Point")),
                ("top_level", "fn", None),
                ("util", "mod", None),
                ("helper", "method", Some("util")),
            ],
            "got: {symbols:#?}"
        );
    }

    const PYTHON_FIXTURE: &str = r#"
class Greeter:
    def __init__(self, name):
        self.name = name

    def greet(self):
        def inner():
            return "hi"
        return inner()

def top_level(x):
    return x
"#;

    #[test]
    fn python_golden_symbol_list() {
        let symbols = extract_symbols("python", PYTHON_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            names(&symbols),
            vec![
                ("Greeter", "class", None),
                ("__init__", "def", Some("Greeter")),
                ("greet", "def", Some("Greeter")),
                ("inner", "def", Some("greet")),
                ("top_level", "def", None),
            ],
            "got: {symbols:#?}"
        );
    }

    const RUBY_FIXTURE: &str = r#"
module Shapes
  class Circle
    def initialize(radius)
      @radius = radius
    end

    def area
      3.14 * @radius * @radius
    end

    def self.unit
      Circle.new(1)
    end
  end
end
"#;

    #[test]
    fn ruby_golden_symbol_list() {
        let symbols = extract_symbols("ruby", RUBY_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            names(&symbols),
            vec![
                ("Shapes", "module", None),
                ("Circle", "class", Some("Shapes")),
                ("initialize", "method", Some("Circle")),
                ("area", "method", Some("Circle")),
                ("unit", "singleton_method", Some("Circle")),
            ],
            "got: {symbols:#?}"
        );
    }

    // --- TypeScript (W2.2) --------------------------------------------

    const TYPESCRIPT_FIXTURE: &str = r#"
interface Shape {
  area(): number;
}

type Point = { x: number; y: number };

enum Color {
  Red,
  Green,
}

namespace Utils {
  export function helper(): number {
    return 1;
  }
}

class Circle implements Shape {
  radius: number;

  constructor(radius: number) {
    this.radius = radius;
  }

  area(): number {
    return 3.14 * this.radius * this.radius;
  }
}

const makePoint = (x: number, y: number): Point => ({ x, y });

function topLevel(a: number): number {
  return a;
}

export const PI = 3.14;
"#;

    /// One of each construct the W2.2 scope calls out by name: interface,
    /// type alias, enum, namespace, class, method, arrow-const, function —
    /// plus an exported plain constant and (deliberately) a constructor,
    /// which kb-code's own vendored query does NOT exclude (unlike
    /// JavaScript's official query's `#not-eq? @name "constructor"` — see
    /// `queries/typescript-tags.scm`'s header on why this file doesn't
    /// mirror that predicate).
    #[test]
    fn typescript_golden_symbol_list() {
        let symbols = extract_symbols("typescript", TYPESCRIPT_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            names(&symbols),
            vec![
                ("Shape", "interface", None),
                ("area", "method", Some("Shape")),
                ("Point", "type_alias", None),
                ("Color", "enum", None),
                ("Utils", "module", None),
                ("helper", "function", Some("Utils")),
                ("Circle", "class", None),
                ("constructor", "method", Some("Circle")),
                ("area", "method", Some("Circle")),
                ("makePoint", "function", None),
                ("topLevel", "function", None),
                ("PI", "const", None),
            ],
            "got: {symbols:#?}"
        );
    }

    /// TSX: a component function declaration alongside JSX syntax — proves
    /// the merged query still resolves function_declaration correctly when
    /// the LANGUAGE_TSX grammar (not LANGUAGE_TYPESCRIPT) is in play.
    #[test]
    fn tsx_component_function_is_indexed() {
        let src = r#"
export function Greeting({ name }: { name: string }) {
  return <div>Hello, {name}</div>;
}
"#;
        let symbols = extract_symbols("tsx", src.as_bytes()).unwrap();
        assert_eq!(
            names(&symbols),
            vec![("Greeting", "function", None)],
            "got: {symbols:#?}"
        );
    }

    // --- JavaScript (W2.2 — official bundled tags.scm, used as-is) -----

    const JAVASCRIPT_FIXTURE: &str = r#"
class Greeter {
  constructor(name) {
    this.name = name;
  }

  greet() {
    return `hi ${this.name}`;
  }
}

function topLevel(a) {
  return a;
}

const makeAdder = (x) => (y) => x + y;
"#;

    /// A plain JS file through the OFFICIAL tree-sitter-javascript
    /// `tags.scm` (bundled as-is, no vendoring — see lang.rs). Notably the
    /// upstream query itself excludes `constructor` via a `#not-eq?`
    /// predicate (a real tree-sitter TEXT predicate, auto-evaluated by
    /// `QueryCursor::matches` — unlike kb-code's OWN TypeScript query
    /// above, which deliberately keeps `constructor`).
    #[test]
    fn javascript_plain_file_golden_symbol_list() {
        let symbols = extract_symbols("javascript", JAVASCRIPT_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            names(&symbols),
            vec![
                ("Greeter", "class", None),
                ("greet", "method", Some("Greeter")),
                ("topLevel", "function", None),
                ("makeAdder", "function", None),
            ],
            "got: {symbols:#?}"
        );
    }

    // --- Bash (W2.2 — hand-written, function definitions only) ---------

    const BASH_FIXTURE: &str = "\
greet() {
  echo \"hi $1\"
}

function shout {
  echo \"$1!!!\"
}

function loud() {
  echo \"$1!!!\"
}
";

    /// Both Bash function-definition surface forms — `name() {}` and
    /// `function name {}`/`function name() {}` — collapse to the SAME
    /// grammar node kind (`function_definition`), so one query pattern
    /// covers all three fixture entries; see `queries/bash-tags.scm`.
    #[test]
    fn bash_golden_symbol_list_both_forms() {
        let symbols = extract_symbols("bash", BASH_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            names(&symbols),
            vec![
                ("greet", "function", None),
                ("shout", "function", None),
                ("loud", "function", None),
            ],
            "got: {symbols:#?}"
        );
    }

    /// Error tolerance (W2.2 scope requirement): tree-sitter ALWAYS yields
    /// a tree, ERROR nodes and all, for genuinely broken input — extraction
    /// must proceed through the partial tree rather than bailing out
    /// entirely. `broken_fn` above is deliberately malformed (an
    /// unbalanced paren before its body); `good_fn` below it must still be
    /// found.
    #[test]
    fn bash_broken_syntax_still_yields_the_intact_function() {
        let src = "\
function broken( {
  echo \"oops\"

good_fn() {
  echo \"still works\"
}
";
        let symbols = extract_symbols("bash", src.as_bytes()).unwrap();
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "good_fn" && s.kind == "function"),
            "expected good_fn to survive a syntax error elsewhere in the file: {symbols:#?}"
        );
    }

    // --- Go (W2.6) ------------------------------------------------------

    const GO_FIXTURE: &str = r#"
package shapes

type Point struct {
	X int
	Y int
}

type Shape interface {
	Area() float64
}

type Alias = int

const MaxRetries = 3

var GlobalCache string

func (p Point) Dist(other Point) float64 {
	return 0.0
}

func (p *Point) Move(dx, dy int) {
	p.X += dx
	p.Y += dy
}

func TopLevel(a int) int {
	return a
}
"#;

    /// One of each construct W2.6's scope calls out by name: struct,
    /// interface, plain type alias, const, var, a value-receiver method, a
    /// pointer-receiver method (both resolving to the SAME container via
    /// `go_container_of`'s pointer_type unwrap), and a top-level func.
    #[test]
    fn go_golden_symbol_list() {
        let symbols = extract_symbols("go", GO_FIXTURE.as_bytes()).unwrap();
        assert_eq!(
            names(&symbols),
            vec![
                ("Point", "struct", None),
                ("Shape", "interface", None),
                ("Alias", "type", None),
                ("MaxRetries", "const", None),
                ("GlobalCache", "var", None),
                ("Dist", "method", Some("Point")),
                ("Move", "method", Some("Point")),
                ("TopLevel", "func", None),
            ],
            "got: {symbols:#?}"
        );
    }

    #[test]
    fn empty_source_yields_no_symbols() {
        for lang in lang::ALL_LANG_IDS {
            assert_eq!(extract_symbols(lang, b"").unwrap(), vec![]);
        }
    }

    #[test]
    fn unsupported_language_errors() {
        let err = extract_symbols("cobol", b"").unwrap_err();
        assert!(matches!(err, LangError::Unsupported(_)), "got: {err:?}");
    }

    #[test]
    fn is_repo_map_symbol_excludes_only_yaml_key_rows() {
        assert!(!is_repo_map_symbol("key"));
        for kind in [
            "fn",
            "method",
            "struct",
            "class",
            "def",
            "function",
            "interface",
            "enum",
            "module",
            "const",
            "func",
            "var",
            "type",
        ] {
            assert!(
                is_repo_map_symbol(kind),
                "{kind} should count for a repo map"
            );
        }
    }

    #[test]
    fn ordinals_are_dense_and_byte_ordered() {
        let symbols = extract_symbols("rust", RUST_FIXTURE.as_bytes()).unwrap();
        for (i, s) in symbols.iter().enumerate() {
            assert_eq!(s.ordinal, i as u32);
        }
    }

    // --- B2: signature + doc capture ---------------------------------------

    fn find<'a>(symbols: &'a [Symbol], name: &str) -> &'a Symbol {
        symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol named {name:?} in {symbols:#?}"))
    }

    #[test]
    fn rust_fn_gets_a_cut_signature_and_its_triple_slash_doc() {
        let src = "/// Adds two integers together.\n\
                    /// Returns their sum.\n\
                    fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let symbols = extract_symbols("rust", src.as_bytes()).unwrap();
        let add = find(&symbols, "add");
        // Cut at the body's `{`, not the whole function — the trailing
        // space right before the cut point is trimmed by
        // `collapse_whitespace`'s own `.trim()`.
        assert_eq!(
            add.signature.as_deref(),
            Some("fn add(a: i32, b: i32) -> i32")
        );
        assert_eq!(
            add.doc.as_deref(),
            Some("Adds two integers together. Returns their sum.")
        );
    }

    #[test]
    fn rust_plain_line_comment_is_not_captured_as_doc() {
        let src = "// just a note, not a doc comment\nfn add() {}\n";
        let symbols = extract_symbols("rust", src.as_bytes()).unwrap();
        assert_eq!(find(&symbols, "add").doc, None);
    }

    #[test]
    fn rust_doc_run_stops_at_a_blank_line() {
        let src = "/// unrelated docs above a blank line\n\nfn add() {}\n";
        let symbols = extract_symbols("rust", src.as_bytes()).unwrap();
        assert_eq!(
            find(&symbols, "add").doc,
            None,
            "a blank line must break the contiguous doc run"
        );
    }

    #[test]
    fn rust_block_doc_comment_strips_leading_stars() {
        let src = "/**\n * Multiplies two integers.\n * That's it.\n */\nfn mul() {}\n";
        let symbols = extract_symbols("rust", src.as_bytes()).unwrap();
        assert_eq!(
            find(&symbols, "mul").doc.as_deref(),
            Some("Multiplies two integers. That's it.")
        );
    }

    #[test]
    fn rust_bodiless_item_signature_is_the_full_node_capped() {
        // A bodiless trait method (`fn area(&self) -> f64;`, a
        // `function_signature_item`) produces NO symbol row at all — the
        // official `tags.scm`'s method/function patterns only target
        // `function_item` (see this module's own top doc and
        // `rust_golden_symbol_list`'s fixture, which asserts the identical
        // thing) — so `type_item` is the real bodiless-item case: it IS
        // indexed (kind `type_alias`) but has no `body` field entry (see
        // `body_field_name`), so the signature is the full node, semicolon
        // included.
        let src = "type Alias = i32;\n";
        let symbols = extract_symbols("rust", src.as_bytes()).unwrap();
        assert_eq!(
            find(&symbols, "Alias").signature.as_deref(),
            Some("type Alias = i32;")
        );
    }

    #[test]
    fn rust_signature_caps_at_200_chars() {
        let params = (0..40)
            .map(|i| format!("p{i}: i32"))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("fn many({params}) {{\n}}\n");
        let symbols = extract_symbols("rust", src.as_bytes()).unwrap();
        let sig = find(&symbols, "many").signature.as_deref().unwrap();
        assert_eq!(sig.chars().count(), SIGNATURE_CAP);
    }

    #[test]
    fn rust_doc_caps_at_400_chars() {
        let long_line = "x".repeat(500);
        let src = format!("/// {long_line}\nfn f() {{}}\n");
        let symbols = extract_symbols("rust", src.as_bytes()).unwrap();
        let doc = find(&symbols, "f").doc.as_deref().unwrap();
        assert_eq!(doc.chars().count(), DOC_CAP);
    }

    #[test]
    fn typescript_jsdoc_is_captured() {
        let src = "/**\n * A friendly greeter.\n */\nfunction greet(name: string): string {\n    return name;\n}\n";
        let symbols = extract_symbols("typescript", src.as_bytes()).unwrap();
        let greet = find(&symbols, "greet");
        assert_eq!(greet.doc.as_deref(), Some("A friendly greeter."));
        assert_eq!(
            greet.signature.as_deref(),
            Some("function greet(name: string): string")
        );
    }

    #[test]
    fn typescript_plain_line_comment_run_is_captured_as_doc() {
        // Unlike Rust, TS/JS has no dedicated doc marker — a contiguous
        // `//` run directly above a declaration counts (see the module
        // doc's "Doc-comment capture" section).
        let src = "// A friendly greeter.\n// Second line.\nfunction greet() {}\n";
        let symbols = extract_symbols("typescript", src.as_bytes()).unwrap();
        assert_eq!(
            find(&symbols, "greet").doc.as_deref(),
            Some("A friendly greeter. Second line.")
        );
    }

    #[test]
    fn arrow_const_signature_is_the_full_node_capped_not_body_cut() {
        // `variable_declarator` (the node the arrow-const tags pattern
        // actually captures) has no `body` field — see `body_field_name`'s
        // doc — so the WHOLE thing, arrow body included, is the signature.
        let src = "const add = (a: number, b: number) => a + b;\n";
        let symbols = extract_symbols("typescript", src.as_bytes()).unwrap();
        assert_eq!(
            find(&symbols, "add").signature.as_deref(),
            Some("add = (a: number, b: number) => a + b")
        );
    }

    #[test]
    fn python_bare_def_has_a_signature_but_no_doc_without_a_docstring() {
        // B5b widened Python into `TOKEN_LEVEL_LANG_IDS` — it now DOES get a
        // signature; `doc` stays `None` absent a docstring (see
        // `python_docstring_is_none_when_the_first_statement_is_not_a_bare_string`
        // for the "wrong first statement" case).
        let src = "def add(a, b):\n    return a + b\n";
        let symbols = extract_symbols("python", src.as_bytes()).unwrap();
        let add = find(&symbols, "add");
        assert_eq!(add.signature.as_deref(), Some("def add(a, b):"));
        assert_eq!(add.doc, None);
    }

    // --- B5b: signature + doc capture, Python/Ruby/Go/Bash ------------------

    #[test]
    fn python_signature_is_cut_at_the_colon_and_docstring_is_captured() {
        let src = "def add(a: int, b: int) -> int:\n    \"\"\"Adds two integers.\n\n    Extra blank-line-separated paragraph, still all ONE docstring node.\n    \"\"\"\n    return a + b\n";
        let symbols = extract_symbols("python", src.as_bytes()).unwrap();
        let add = find(&symbols, "add");
        assert_eq!(
            add.signature.as_deref(),
            Some("def add(a: int, b: int) -> int:")
        );
        assert_eq!(
            add.doc.as_deref(),
            Some(
                "Adds two integers. Extra blank-line-separated paragraph, still all ONE docstring node."
            )
        );
    }

    #[test]
    fn python_docstring_is_none_when_the_first_statement_is_not_a_bare_string() {
        let src = "def add(a, b):\n    x = 1\n    return a + b + x\n";
        let symbols = extract_symbols("python", src.as_bytes()).unwrap();
        assert_eq!(find(&symbols, "add").doc, None);
    }

    #[test]
    fn python_class_docstring_is_captured_too() {
        let src = "class Greeter:\n    \"\"\"A friendly greeter.\"\"\"\n    def greet(self):\n        pass\n";
        let symbols = extract_symbols("python", src.as_bytes()).unwrap();
        assert_eq!(
            find(&symbols, "Greeter").doc.as_deref(),
            Some("A friendly greeter.")
        );
    }

    #[test]
    fn ruby_method_gets_a_cut_signature_and_a_preceding_comment_run_as_doc() {
        let src = "# Computes the area.\n# Circle only.\ndef area(radius)\n  3.14 * radius * radius\nend\n";
        let symbols = extract_symbols("ruby", src.as_bytes()).unwrap();
        let area = find(&symbols, "area");
        assert_eq!(area.signature.as_deref(), Some("def area(radius)"));
        assert_eq!(area.doc.as_deref(), Some("Computes the area. Circle only."));
    }

    #[test]
    fn ruby_doc_run_stops_at_a_blank_line() {
        let src = "# unrelated\n\ndef area(radius)\n  radius\nend\n";
        let symbols = extract_symbols("ruby", src.as_bytes()).unwrap();
        assert_eq!(find(&symbols, "area").doc, None);
    }

    #[test]
    fn go_func_gets_a_cut_signature_and_a_preceding_comment_as_doc() {
        let src = "// TopLevel does a thing.\nfunc TopLevel(a int) int {\n\treturn a\n}\n";
        let symbols = extract_symbols("go", src.as_bytes()).unwrap();
        let top_level = find(&symbols, "TopLevel");
        assert_eq!(
            top_level.signature.as_deref(),
            Some("func TopLevel(a int) int")
        );
        assert_eq!(top_level.doc.as_deref(), Some("TopLevel does a thing."));
    }

    #[test]
    fn go_bodiless_symbol_signature_is_the_full_node_capped() {
        // `const_spec`/`var_spec`/`type_spec` have no `body` field — the
        // full node text is the signature, same fallback Rust's bodiless
        // items use.
        let src = "package p\n\nconst MaxRetries = 3\n";
        let symbols = extract_symbols("go", src.as_bytes()).unwrap();
        assert_eq!(
            find(&symbols, "MaxRetries").signature.as_deref(),
            Some("MaxRetries = 3")
        );
    }

    #[test]
    fn bash_function_gets_a_cut_signature_and_a_preceding_comment_as_doc() {
        let src = "# Greets someone by name.\ngreet() {\n  echo \"hi $1\"\n}\n";
        let symbols = extract_symbols("bash", src.as_bytes()).unwrap();
        let greet = find(&symbols, "greet");
        assert_eq!(greet.signature.as_deref(), Some("greet()"));
        assert_eq!(greet.doc.as_deref(), Some("Greets someone by name."));
    }

    // --- Phase N: TODO extraction ----------------------------------------

    #[test]
    fn extract_todos_rust_fixture_finds_comment_markers_not_string_literals() {
        let src = r#"
// TODO: wire up the sink
fn decoy() {
    let s = "TODO inside a string must not match";
    // FIXME please
    /* HACK: multi
       line is one comment node — markers only on first scanned line of text */
    // XXX
    // BUG trailing
}
"#;
        let todos = extract_todos("rust", src.as_bytes()).unwrap();
        let markers: Vec<&str> = todos.iter().map(|t| t.marker.as_str()).collect();
        assert!(
            markers.contains(&"TODO"),
            "expected TODO from line comment: {todos:#?}"
        );
        assert!(markers.contains(&"FIXME"), "expected FIXME: {todos:#?}");
        assert!(markers.contains(&"HACK"), "expected HACK: {todos:#?}");
        assert!(markers.contains(&"XXX"), "expected XXX: {todos:#?}");
        assert!(markers.contains(&"BUG"), "expected BUG: {todos:#?}");
        // String-literal "TODO" must NOT appear (comment-node scan only).
        let texts: Vec<&str> = todos.iter().map(|t| t.text.as_str()).collect();
        assert!(
            !texts.iter().any(|t| t.contains("inside a string")),
            "string-literal decoy must not match: {todos:#?}"
        );
        let todo = todos.iter().find(|t| t.marker == "TODO").unwrap();
        assert_eq!(todo.text, ": wire up the sink");
        assert!(todo.line >= 2, "1-based line of the comment: {}", todo.line);
    }

    #[test]
    fn extract_todos_typescript_fixture_finds_comment_markers_not_string_literals() {
        let src = r#"
// TODO: port the reader
export function decoy() {
  const s = "TODO inside a string must not match";
  // FIXME later
  /* XXX: block comment */
  // BUG found
}
"#;
        let todos = extract_todos("typescript", src.as_bytes()).unwrap();
        let markers: Vec<&str> = todos.iter().map(|t| t.marker.as_str()).collect();
        assert!(markers.contains(&"TODO"), "{todos:#?}");
        assert!(markers.contains(&"FIXME"), "{todos:#?}");
        assert!(markers.contains(&"XXX"), "{todos:#?}");
        assert!(markers.contains(&"BUG"), "{todos:#?}");
        assert!(
            !todos.iter().any(|t| t.text.contains("inside a string")),
            "string-literal decoy must not match: {todos:#?}"
        );
        let todo = todos.iter().find(|t| t.marker == "TODO").unwrap();
        assert_eq!(todo.text, ": port the reader");
    }

    #[test]
    fn extract_todos_skips_outline_tier_languages() {
        assert!(extract_todos("yaml", b"# TODO: no\nkey: 1\n")
            .unwrap()
            .is_empty());
        assert!(extract_todos("json", b"{}\n").unwrap().is_empty());
        assert!(extract_todos("toml", b"# TODO x\n").unwrap().is_empty());
    }

    #[test]
    fn extract_todos_requires_word_boundary() {
        // "TODOS" and "myTODO" must not match the TODO marker.
        let src = "// TODOS more\n// myTODO\n// TODO real\n";
        let todos = extract_todos("rust", src.as_bytes()).unwrap();
        assert_eq!(todos.len(), 1, "{todos:#?}");
        assert_eq!(todos[0].marker, "TODO");
        assert_eq!(todos[0].text, "real");
    }

    #[test]
    fn extract_todos_caps_trailing_text_at_200_chars() {
        let long = "x".repeat(300);
        let src = format!("// TODO {long}\n");
        let todos = extract_todos("rust", src.as_bytes()).unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].text.chars().count(), TODO_TEXT_CAP);
    }
}
