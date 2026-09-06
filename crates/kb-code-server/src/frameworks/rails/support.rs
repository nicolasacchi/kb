//! PRR-N4 — shared Ruby-AST + inflection helpers for every N4 extractor
//! (`view_component.rs`/`stimulus.rs`/`models.rs`/`jobs_mailers.rs`/
//! `specs.rs`/`i18n.rs`/`helpers.rs`, plus `routes.rs`'s N4 `devise_for`
//! extension).
//!
//! # Why this exists (a deliberate departure from N3's own precedent)
//!
//! `routes.rs` and `views.rs` (PRR-N3) each carry a PRIVATE copy of the same
//! ~8 small tree-sitter helpers (`call_method_name`/`call_args`/
//! `literal_string_or_symbol`/…) and `views.rs`'s own doc explicitly defends
//! that as deliberate ("small, deliberately NOT shared with routes.rs — see
//! this module's PR report for the duplication tradeoff") — a reasonable
//! call at TWO files. PRR-N4 adds SEVEN more extractor files that all need
//! the identical walk primitives; duplicating the same ~9 functions seven
//! more times crosses from "a little duplication buys module independence"
//! into "the same bug now has eight places to fix." This module is the
//! honest simplification at that new file count. `routes.rs`/`views.rs`
//! themselves are left untouched (their own private copies, their own
//! golden-pinned tests) — refactoring already-shipped, golden-tested N3 code
//! for a purely cosmetic DRY win isn't worth the regression risk; every N4
//! extractor below imports from here instead.
//!
//! Every function here is a pure, source-text-only helper — no I/O, no
//! `Store`/`Path` awareness (that lives in each extractor's own resolution
//! logic, mirroring `views.rs`'s `find_view_files`).

use crate::frameworks::FrameworkEdge;
use tree_sitter::Node;

// --- shared tree walks (ERB fragment + call-site) --------------------------

/// Walk an ERB tree (`tree-sitter-embedded-template`'s CST) and invoke
/// `scan` on every `directive`/`output_directive` tag's INDEPENDENTLY
/// re-parsed Ruby fragment — the same per-tag injection `views.rs`'s
/// `walk_erb_template`/`scan_calls` establishes (see that module's doc for
/// the "accepted ceiling": no control flow spanning multiple tags).
/// Generalized here so every N4 Ruby-fragment scanner (`view_component`/
/// `jobs_mailers`/`i18n`) shares ONE ERB walk instead of reimplementing it.
/// `comment_directive` (`<%# … %>`) is never matched — dead code, never
/// executed, exactly like `views.rs`'s own walk.
///
/// `scan(ruby_root, ruby_source, line_offset, out)` mirrors `views.rs`'s
/// `scan_calls` signature: `line_offset` is the `code` node's absolute row
/// within the WHOLE `.erb` file (the re-parsed fragment's own nodes count
/// from row 0 again), so callers can re-anchor `src_line` the same way
/// `views.rs::src_line` does.
///
/// `scan`'s `Node` parameter is deliberately UNNAMED-lifetime (a fresh
/// higher-ranked `for<'r>` per call, not tied to `node`'s own lifetime) —
/// each re-parsed Ruby fragment (`ruby_tree` below) is a NEW, function-
/// locally-owned `Tree`, whose `root_node()` lifetime can be shorter than
/// `node`'s. A named type alias here would have to spell out that same
/// `for<'r>` HRTB explicitly (type aliases don't get a function
/// signature's automatic elision-to-HRTB treatment) — clippy's
/// `type_complexity` lint is allowed instead of chasing that HRTB syntax,
/// which is a well-known Rust ergonomics rough edge for this exact
/// "callback accepts a locally-created tree's node" shape; the signature
/// stays exactly what a `dyn FnMut` reader expects.
#[allow(clippy::type_complexity)]
pub fn walk_erb_ruby_fragments(
    node: Node,
    source: &[u8],
    out: &mut Vec<FrameworkEdge>,
    scan: &mut dyn FnMut(Node, &[u8], u32, &mut Vec<FrameworkEdge>),
) {
    // V72-H2a — the WALK moved to `crate::injection` (the one place a
    // host's guest regions are located); this function is now the
    // per-region re-parse + `scan` invocation and nothing else. The
    // region list is the same preorder over the same `code` children with
    // the same `start_position().row`, so the rails-lens goldens are
    // unchanged.
    for region in crate::injection::erb_regions(node, source) {
        if region.kind != crate::injection::RegionKind::Fragment {
            continue;
        }
        let Ok((ruby_tree, _language)) = crate::lang::parse(region.lang, region.source.as_bytes())
        else {
            continue;
        };
        // `line_offset` is the region's own 0-based host row — exactly
        // what `support::src_line` adds 1 to.
        let offset = match &region.map {
            crate::injection::OffsetMap::Shift { row_base, .. } => *row_base,
            crate::injection::OffsetMap::Lines(_) => continue,
        };
        scan(ruby_tree.root_node(), region.source.as_bytes(), offset, out);
    }
}

/// The HAML counterpart of [`walk_erb_ruby_fragments`] — and the reason
/// V72-H3 needed no second Rails extractor.
///
/// `.haml` has no tree-sitter grammar, so there is no per-tag `code` node
/// to re-parse. `crate::haml` instead produces ONE synthesized Ruby
/// program out of the whole template (every script line, interpolation,
/// attribute hash and object reference, with `end`s derived from the
/// indentation tree) plus a LINE MAP back to HAML lines. This walk parses
/// that program once and hands its root to the SAME `scan` callback the
/// ERB walk uses, so every caller — `views`, `i18n`, `view_component`,
/// `jobs_mailers` — resolves HAML call sites with byte-identical logic and
/// mints the same edge kinds at the same trust classes.
///
/// Two differences from the ERB walk, both deliberate:
///
/// * `line_offset` is `0`. The program's own rows ARE the addressing unit,
///   and the re-anchoring happens once, HERE, after `scan` returns —
///   rather than being folded into an offset the callback would have to
///   understand. Every callback stays unchanged.
/// * an edge whose program row maps to a SYNTHETIC line (an `end` this
///   module invented) is DROPPED. No call site can live on such a line, so
///   this is unreachable in practice; the alternative — emitting the edge
///   with a borrowed neighbouring line — would be a fabricated location,
///   which this crate does not do.
///
/// Unlike ERB's per-tag re-parse, this DOES reconstruct control flow
/// spanning multiple constructs (`- if` … `- else` is one Ruby `if`), so
/// the "accepted ceiling" `views`'s module doc records for ERB does not
/// apply to HAML.
#[allow(clippy::type_complexity)]
pub fn walk_haml_ruby_fragments(
    source: &[u8],
    out: &mut Vec<FrameworkEdge>,
    scan: &mut dyn FnMut(Node, &[u8], u32, &mut Vec<FrameworkEdge>),
) {
    let src = match std::str::from_utf8(source) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&source[..e.valid_up_to()]).unwrap_or(""),
    };
    // V72-H2a — the synthesized program is now `crate::injection`'s ONE
    // `Program` region for a HAML host; `ruby_program` still builds it,
    // and the re-anchoring below still goes through its line map, now
    // named `OffsetMap::host_line`. Byte-identical output.
    let doc = crate::haml::parser::parse_str(src);
    let Some(program) = crate::injection::haml_regions(&doc, src)
        .into_iter()
        .find(|r| r.kind == crate::injection::RegionKind::Program)
    else {
        return;
    };
    let Ok((tree, _language)) = crate::lang::parse(program.lang, program.source.as_bytes()) else {
        return;
    };
    let mut minted = Vec::new();
    scan(tree.root_node(), program.source.as_bytes(), 0, &mut minted);
    for mut edge in minted {
        match edge.src_line {
            // `src_line` is 1-based (`support::src_line` adds the 1), so
            // the program ROW is one less.
            Some(line) => match program.map.host_line(line.saturating_sub(1)) {
                Some(haml_line) => {
                    edge.src_line = Some(haml_line);
                    out.push(edge);
                }
                None => continue,
            },
            // A file-level edge has no line to re-anchor.
            None => out.push(edge),
        }
    }
}

/// Recursively walk `node`'s whole subtree for `call` nodes (a render call
/// nested inside `if`/`respond_to`/`case` is still a real call site — see
/// `views.rs::scan_calls`'s identical doc) and invoke `resolve` on each.
/// See [`walk_erb_ruby_fragments`]'s doc for why `clippy::type_complexity`
/// is allowed here too (same "callback's `Node` needs its own free
/// higher-ranked lifetime" shape — `walk_calls` is called directly on a
/// freshly-parsed tree's root by several callers, same as
/// `walk_erb_ruby_fragments`'s inner `scan` invocation).
#[allow(clippy::type_complexity)]
pub fn walk_calls(
    node: Node,
    source: &[u8],
    line_offset: u32,
    out: &mut Vec<FrameworkEdge>,
    resolve: &mut dyn FnMut(Node, &[u8], u32) -> Vec<FrameworkEdge>,
) {
    if node.kind() == "call" {
        out.extend(resolve(node, source, line_offset));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(child, source, line_offset, out, resolve);
    }
}

// --- call-site shape -------------------------------------------------------

pub fn src_line(node: Node, line_offset: u32) -> u32 {
    node.start_position().row as u32 + line_offset + 1
}

pub fn call_method_name(node: Node, source: &[u8]) -> Option<String> {
    let m = node.child_by_field_name("method")?;
    m.utf8_text(source).ok().map(|s| s.to_string())
}

pub fn call_args(node: Node<'_>) -> Vec<Node<'_>> {
    let Some(list) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor).collect()
}

pub fn call_block_body(node: Node<'_>) -> Option<Node<'_>> {
    let block = node.child_by_field_name("block")?;
    block_body_node(block)
}

pub fn block_body_node(block: Node<'_>) -> Option<Node<'_>> {
    match block.kind() {
        "do_block" | "block" => block.child_by_field_name("body"),
        _ => None,
    }
}

/// Push `node` onto `opts` if it's a bare `pair` (trailing-hash-without-
/// braces shape, e.g. `to: :target`); if it's an explicit `hash` literal
/// (`{to: :target}`), flatten its `pair` children in instead. Anything else
/// is ignored — see `routes.rs`'s original doc for the same shape.
pub fn collect_pairs_into<'a>(node: Node<'a>, opts: &mut Vec<Node<'a>>) {
    match node.kind() {
        "pair" => opts.push(node),
        "hash" => {
            let mut cursor = node.walk();
            for c in node.named_children(&mut cursor) {
                if c.kind() == "pair" {
                    opts.push(c);
                }
            }
        }
        _ => {}
    }
}

pub fn pair_key_name(pair: Node, source: &[u8]) -> Option<String> {
    let key = pair.child_by_field_name("key")?;
    match key.kind() {
        "hash_key_symbol" => key
            .utf8_text(source)
            .ok()
            .map(|s| s.trim_end_matches(':').to_string()),
        "simple_symbol" => literal_symbol(key, source),
        "string" => literal_string(key, source),
        "identifier" => key.utf8_text(source).ok().map(|s| s.to_string()),
        _ => None,
    }
}

pub fn find_pair_node<'a>(opts: &[Node<'a>], key: &str, source: &[u8]) -> Option<Node<'a>> {
    opts.iter()
        .find(|p| pair_key_name(**p, source).as_deref() == Some(key))
        .copied()
}

pub fn find_pair_literal(opts: &[Node], key: &str, source: &[u8]) -> Option<String> {
    let value = find_pair_node(opts, key, source)?.child_by_field_name("value")?;
    literal_string_or_symbol(value, source)
}

/// A plain (non-interpolated) string or symbol literal's text value —
/// `None` for anything else, INCLUDING an interpolated string (never treat
/// `"#{x}"` as a literal).
pub fn literal_string_or_symbol(node: Node, source: &[u8]) -> Option<String> {
    literal_string(node, source).or_else(|| literal_symbol(node, source))
}

pub fn literal_string(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    if node
        .children(&mut cursor)
        .any(|c| c.kind() == "interpolation")
    {
        return None;
    }
    let text = node.utf8_text(source).ok()?;
    if text.len() < 2 {
        return None;
    }
    Some(text[1..text.len() - 1].to_string())
}

pub fn literal_symbol(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "simple_symbol" => {
            let text = node.utf8_text(source).ok()?;
            Some(text.trim_start_matches(':').to_string())
        }
        "delimited_symbol" => {
            let mut cursor = node.walk();
            if node
                .children(&mut cursor)
                .any(|c| c.kind() == "interpolation")
            {
                return None;
            }
            let text = node.utf8_text(source).ok()?;
            let after_colon = text.strip_prefix(':')?;
            if after_colon.len() < 2 {
                return None;
            }
            Some(after_colon[1..after_colon.len() - 1].to_string())
        }
        _ => None,
    }
}

/// A bare Ruby constant reference (`Order`, no receiver — tree-sitter-ruby's
/// `constant` node kind) — used for `class_name: Order`-shaped (unquoted)
/// literals, which are legal Ruby even though `class_name: "Order"` is the
/// far more common convention. `scope_resolution`/`::`-qualified constants
/// (`Trade::Order`) are handled too: their text already contains the `::`,
/// which callers convert to a `/` path segment the same way `routes.rs`
/// converts `module_prefix` joins.
pub fn constant_text(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "constant" | "scope_resolution" => node.utf8_text(source).ok().map(|s| s.to_string()),
        _ => None,
    }
}

// --- inflection --------------------------------------------------------

/// Simple English pluralizer (moved verbatim from `routes.rs`'s original
/// PRR-N3 copy — see `singularize`'s doc for why both now live here).
/// Covers the common cases (`s`/`x`/`ch`/`sh` → `+es`, consonant+`y` →
/// `ies`, else `+s`) — an honest, documented approximation, not a full
/// inflector.
pub fn pluralize(name: &str) -> String {
    if name.ends_with('s') || name.ends_with('x') || name.ends_with("ch") || name.ends_with("sh") {
        format!("{name}es")
    } else if name.ends_with('y') && !ends_with_vowel_then_y(name) {
        format!("{}ies", &name[..name.len() - 1])
    } else {
        format!("{name}s")
    }
}

/// The inverse heuristic of [`pluralize`] — needed by `models.rs`'s
/// `association` default `class_name` guess (`has_many :orders` → model
/// `Order`, singular). Approximate in the SAME honest sense as `pluralize`:
/// `ies` → `y` (consonant+y pluralization reversed), `ses`/`xes`/`ches`/
/// `shes` → drop the trailing `es`, else a trailing `s` is dropped. Never
/// claims to handle irregulars (`people`→`person`, `data`→`datum`) — those
/// fall through unchanged, same honesty stance as `pluralize`'s own doc.
pub fn singularize(name: &str) -> String {
    if let Some(stem) = name.strip_suffix("ies") {
        if !stem.is_empty() {
            return format!("{stem}y");
        }
    }
    for suffix in ["ses", "xes", "ches", "shes"] {
        if let Some(stem) = name.strip_suffix(suffix) {
            if !stem.is_empty() {
                return format!("{stem}{}", &suffix[..1]);
            }
        }
    }
    name.strip_suffix('s').unwrap_or(name).to_string()
}

fn ends_with_vowel_then_y(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 2 {
        return false;
    }
    matches!(bytes[bytes.len() - 2], b'a' | b'e' | b'i' | b'o' | b'u')
}

/// `snake_case`/`under_scored` name → `CamelCase` (Rails' `classify`/
/// `camelize`), one segment at a time — `/`-joined path segments (already
/// split by the caller, e.g. `join_path`'s output) are NOT this function's
/// concern; it only camelizes a single bare segment (`"round"` →
/// `"Round"`, `"api_key"` → `"ApiKey"`).
pub fn camelize_segment(name: &str) -> String {
    name.split('_')
        .filter(|p| !p.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// `CamelCase`/`snake_case`-mixed name → `snake_case` (Rails' `underscore`),
/// one segment at a time. Handles the common ASCII case (an uppercase
/// letter starts a new word, boundary gets a `_`); does not attempt
/// acronym-run splitting beyond the standard `(?<=[a-z0-9])(?=[A-Z])` +
/// `(?<=[A-Z])(?=[A-Z][a-z])` regex-equivalent rule Rails' own `underscore`
/// uses — close enough for model/component/job class names, which are
/// written in plain CamelCase in practice.
pub fn underscore_segment(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let chars: Vec<char> = name.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_is_lower_or_digit =
                i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
            let next_is_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let prev_is_upper = i > 0 && chars[i - 1].is_uppercase();
            if i > 0 && (prev_is_lower_or_digit || (prev_is_upper && next_is_lower)) {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// A full (possibly `::`-namespaced) Ruby constant name → its Rails
/// convention source-relative path UNDER `prefix` (e.g. `"app/models"`),
/// e.g. `"Trade::Order"` + `"app/models"` → `"app/models/trade/order.rb"`.
/// Each `::`-separated segment is underscored independently (matches
/// Rails' `underscore` applied to the whole namespaced string, which is
/// equivalent to underscoring each segment and joining with `/`).
pub fn const_to_path(const_name: &str, prefix: &str) -> String {
    let segments: Vec<String> = const_name.split("::").map(underscore_segment).collect();
    format!("{prefix}/{}.rb", segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pluralize_matches_routes_rs_original_cases() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("class"), "classes");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("session"), "sessions");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("day"), "days"); // vowel+y: no ies
    }

    #[test]
    fn singularize_is_pluralizes_honest_inverse() {
        assert_eq!(singularize("orders"), "order");
        assert_eq!(singularize("categories"), "category");
        assert_eq!(singularize("boxes"), "box");
        assert_eq!(singularize("classes"), "class");
        assert_eq!(singularize("days"), "day");
        for name in ["order", "category", "box", "class", "day"] {
            assert_eq!(singularize(pluralize(name).as_str()), name, "{name}");
        }
    }

    #[test]
    fn camelize_segment_underscored_to_camel() {
        assert_eq!(camelize_segment("round"), "Round");
        assert_eq!(camelize_segment("api_key"), "ApiKey");
        assert_eq!(
            camelize_segment("order_acube_document_log"),
            "OrderAcubeDocumentLog"
        );
    }

    #[test]
    fn underscore_segment_camel_to_underscored() {
        assert_eq!(underscore_segment("Round"), "round");
        assert_eq!(underscore_segment("ApiKey"), "api_key");
        assert_eq!(underscore_segment("HTTPResponse"), "http_response");
        assert_eq!(
            underscore_segment("OrderAcubeDocumentLog"),
            "order_acube_document_log"
        );
    }

    #[test]
    fn const_to_path_handles_namespaced_constants() {
        assert_eq!(const_to_path("Order", "app/models"), "app/models/order.rb");
        assert_eq!(
            const_to_path("Trade::Order", "app/models"),
            "app/models/trade/order.rb"
        );
    }
}
