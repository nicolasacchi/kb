//! V71-E1 — what a usage row is DOING, from the CST (`usages/2`'s `kind`).
//!
//! `access.rs` answers read-vs-write for four languages and returns `None`
//! when unsure; this module answers the wider question D4's closed
//! vocabulary asks (`call` · `instantiate` · `include` · `inherit` ·
//! `rescue` · `alias` · `mutate` …) and follows exactly the same refusal
//! rule: **`None` means unclassified, and `unclassified` is a first-class,
//! honest outcome that must never be silently coerced into `call`.**
//!
//! # One parse per FILE, not per row
//!
//! [`classify_file`] takes every position in a file at once and walks the
//! tree once. That is deliberate: `usages.rs`'s existing per-row
//! `access_at` + `call_arg_count_at` pair re-parses the containing file
//! twice PER ROW (recon `usages.md` §6.1), and the v2 enrichment must not
//! multiply that. A row's kind therefore costs one parse per distinct file
//! in the result page.
//!
//! # Coverage is honest, not uniform
//!
//! Ruby — the v7.1 target corpus — gets the full parent-shape table.
//! Every other token-level language gets `call` and `instantiate` only;
//! their `def`/`import`/`read`/`write` kinds are derived by the CALLER from
//! data it already has (the occurrence `role`, and `access.rs`'s answer),
//! with no second parse and no new claim. The remaining vocabulary entries
//! are unmintable today and are listed, with their reasons, by the
//! `usages2` dead-surface test — a declared kind with no mint site is
//! exactly the v7.0 "silently dead surface" defect, so it is written down
//! rather than left to be discovered.
//!
//! A kind is never a trust claim of its own: it rides its row's class, and
//! a convention-derived kind (the Rails lens) rides a row that is
//! structurally capped below `exact` (`migrations/V0026`'s CHECK).

use crate::usages2::UsageKind;

/// Classify each `(line, col)` position (1-based line, 0-based byte col)
/// in `source`, parsed as `lang_id`. The returned vector is parallel to
/// `positions`; `None` = "no provable kind here" (the caller's
/// `unclassified`, or its own role/access-derived fallback).
///
/// A parse failure yields all-`None` — never a guess.
pub fn classify_file(
    lang_id: &str,
    source: &[u8],
    positions: &[(u32, u32)],
) -> Vec<Option<UsageKind>> {
    let mut out = vec![None; positions.len()];
    let Ok((tree, _)) = crate::lang::parse(lang_id, source) else {
        return out;
    };
    let root = tree.root_node();
    for (i, &(line, col)) in positions.iter().enumerate() {
        let point = tree_sitter::Point {
            row: line.saturating_sub(1) as usize,
            column: col as usize,
        };
        let Some(node) = root.named_descendant_for_point_range(point, point) else {
            continue;
        };
        let Some(leaf) = name_leaf(node, point) else {
            continue;
        };
        out[i] = match lang_id {
            "ruby" => classify_ruby(leaf, source),
            _ => classify_generic(lang_id, leaf, source),
        };
    }
    out
}

/// The identifier-ish leaf covering `point` — either `node` itself or one
/// of its children (the same climb `access::access_at` performs).
fn name_leaf<'t>(
    node: tree_sitter::Node<'t>,
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node<'t>> {
    if is_name_leaf(node) {
        return Some(node);
    }
    let mut c = node.walk();
    // NOT `node.named_children(&mut c).find(...)` as the tail expression:
    // the iterator borrows `c`, and returning it directly here trips E0597
    // ("c does not live long enough") — `c` is dropped at the end of this
    // block, before the borrow the tail-expression temporary would need.
    // Binding the `find` result to an owned local first (rustc's own
    // suggested fix for this shape) drops the borrow before `c` does.
    let found = node.named_children(&mut c).find(|&child| {
        child.start_position() <= point && point < child.end_position() && is_name_leaf(child)
    });
    found
}

fn is_name_leaf(node: tree_sitter::Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier"
            | "constant"
            | "property_identifier"
            | "field_identifier"
            | "type_identifier"
            | "shorthand_property_identifier"
    )
}

/// Ruby's parent-shape table. Every arm is a shape the CST proves; there
/// is no "probably a call" fallback (Ruby cannot distinguish a bare local
/// read from a receiver-less method call without binding information the
/// caller holds, so that case stays `None`).
fn classify_ruby(leaf: tree_sitter::Node<'_>, source: &[u8]) -> Option<UsageKind> {
    let parent = leaf.parent()?;
    match parent.kind() {
        // `class Foo < Bar` — the superclass expression.
        "superclass" => return Some(UsageKind::Inherit),
        // `rescue Foo, Bar => e`
        "exceptions" => return Some(UsageKind::Rescue),
        // `alias new_name old_name`
        "alias" => return Some(UsageKind::Alias),
        // `x = …` / `a, b = …`
        "assignment"
        | "left_assignment_list"
        | "destructured_left_assignment"
        | "rest_assignment" => {
            if is_in_field(parent, "left", leaf) || parent.kind() != "assignment" {
                return Some(UsageKind::Write);
            }
        }
        // `x += 1`, `x <<= …`
        "operator_assignment" => {
            if is_in_field(parent, "left", leaf) {
                return Some(UsageKind::Mutate);
            }
        }
        // `x << y` — Understand's Modify, on the receiver side only.
        "binary" => {
            let op = parent
                .child_by_field_name("operator")
                .and_then(|n| n.utf8_text(source).ok());
            if op == Some("<<") && is_in_field(parent, "left", leaf) {
                return Some(UsageKind::Mutate);
            }
        }
        _ => {}
    }
    if parent.kind() == "call" {
        return classify_ruby_call(parent, leaf, source);
    }
    // An argument to a receiver-less `include`/`extend`/`prepend`.
    if let Some(mixin) = ruby_mixin_arg(leaf, source) {
        return Some(mixin);
    }
    None
}

fn classify_ruby_call(
    call: tree_sitter::Node<'_>,
    leaf: tree_sitter::Node<'_>,
    source: &[u8],
) -> Option<UsageKind> {
    let method = call.child_by_field_name("method");
    let mname = method.and_then(|n| n.utf8_text(source).ok()).unwrap_or("");
    let is_method_pos = method == Some(leaf);
    let is_receiver_pos = call.child_by_field_name("receiver") == Some(leaf);
    if is_receiver_pos {
        // `Foo.new` — the receiver is what is being instantiated.
        if mname == "new" {
            return Some(UsageKind::Instantiate);
        }
        // `list.compact!` — a bang method mutates its receiver.
        if mname.ends_with('!') {
            return Some(UsageKind::Mutate);
        }
        return Some(UsageKind::Read);
    }
    if is_method_pos {
        return match mname {
            "include" => Some(UsageKind::Include),
            "extend" => Some(UsageKind::Extend),
            "prepend" => Some(UsageKind::Prepend),
            _ => Some(UsageKind::Call),
        };
    }
    None
}

/// `include Foo` / `extend Foo` / `prepend Foo` — `leaf` is the ARGUMENT.
fn ruby_mixin_arg(leaf: tree_sitter::Node<'_>, source: &[u8]) -> Option<UsageKind> {
    let mut cur = leaf;
    // constant → (scope_resolution) → argument_list → call
    for _ in 0..3 {
        let parent = cur.parent()?;
        if parent.kind() == "argument_list" {
            let call = parent.parent()?;
            if call.kind() != "call" || call.child_by_field_name("receiver").is_some() {
                return None;
            }
            let mname = call.child_by_field_name("method")?.utf8_text(source).ok()?;
            return match mname {
                "include" => Some(UsageKind::Include),
                "extend" => Some(UsageKind::Extend),
                "prepend" => Some(UsageKind::Prepend),
                _ => None,
            };
        }
        cur = parent;
    }
    None
}

/// The non-Ruby languages: `call` and `instantiate` only — the two shapes
/// worth a parse that the caller cannot derive from `role`/`access`.
fn classify_generic(
    lang_id: &str,
    leaf: tree_sitter::Node<'_>,
    _source: &[u8],
) -> Option<UsageKind> {
    let parent = leaf.parent()?;
    match lang_id {
        "rust" => {
            if parent.kind() == "call_expression" && is_in_field(parent, "function", leaf) {
                return Some(UsageKind::Call);
            }
            // `foo.bar()` / `Foo::bar()` — the name sits one node down.
            if matches!(parent.kind(), "field_expression" | "scoped_identifier") {
                let gp = parent.parent()?;
                if gp.kind() == "call_expression" && is_in_field(gp, "function", parent) {
                    return Some(UsageKind::Call);
                }
            }
        }
        "typescript" | "tsx" | "javascript" => {
            // `call_expression` names its callee `function`;
            // `new_expression` names it `constructor` (tree-sitter-
            // javascript's grammar) — reading the wrong field here would
            // silently classify every `new Foo()` as unclassified.
            if let Some(k) = ts_callee_kind(parent, leaf) {
                return Some(k);
            }
            if parent.kind() == "member_expression" {
                let gp = parent.parent()?;
                if let Some(k) = ts_callee_kind(gp, parent) {
                    return Some(k);
                }
            }
        }
        "python" => {
            if parent.kind() == "call" && is_in_field(parent, "function", leaf) {
                return Some(UsageKind::Call);
            }
            if parent.kind() == "attribute" {
                let gp = parent.parent()?;
                if gp.kind() == "call" && is_in_field(gp, "function", parent) {
                    return Some(UsageKind::Call);
                }
            }
        }
        "go" => {
            if parent.kind() == "call_expression" && is_in_field(parent, "function", leaf) {
                return Some(UsageKind::Call);
            }
            if parent.kind() == "selector_expression" {
                let gp = parent.parent()?;
                if gp.kind() == "call_expression" && is_in_field(gp, "function", parent) {
                    return Some(UsageKind::Call);
                }
            }
        }
        _ => {}
    }
    None
}

/// `call_expression`/`new_expression` callee test for the TS/JS family.
fn ts_callee_kind(
    parent: tree_sitter::Node<'_>,
    child: tree_sitter::Node<'_>,
) -> Option<UsageKind> {
    match parent.kind() {
        "call_expression" if is_in_field(parent, "function", child) => Some(UsageKind::Call),
        "new_expression" if is_in_field(parent, "constructor", child) => {
            Some(UsageKind::Instantiate)
        }
        _ => None,
    }
}

/// `true` when `child` IS `parent`'s `field`, or sits inside it.
fn is_in_field(parent: tree_sitter::Node<'_>, field: &str, child: tree_sitter::Node<'_>) -> bool {
    let Some(f) = parent.child_by_field_name(field) else {
        return false;
    };
    f == child || (f.start_byte() <= child.start_byte() && child.end_byte() <= f.end_byte())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_at(lang: &str, src: &str, line: u32, col: u32) -> Option<UsageKind> {
        classify_file(lang, src.as_bytes(), &[(line, col)]).remove(0)
    }

    #[test]
    fn ruby_call_receiver_and_instantiation() {
        let src = "order.total\nInvoice.new(order)\nitems.compact!\n";
        assert_eq!(kind_at("ruby", src, 1, 6), Some(UsageKind::Call));
        assert_eq!(kind_at("ruby", src, 1, 0), Some(UsageKind::Read));
        assert_eq!(kind_at("ruby", src, 2, 0), Some(UsageKind::Instantiate));
        assert_eq!(kind_at("ruby", src, 3, 0), Some(UsageKind::Mutate));
    }

    #[test]
    fn ruby_write_and_mutate_assignments() {
        let src = "total = 1\ntotal += 2\nlist << 3\n";
        assert_eq!(kind_at("ruby", src, 1, 0), Some(UsageKind::Write));
        assert_eq!(kind_at("ruby", src, 2, 0), Some(UsageKind::Mutate));
        assert_eq!(kind_at("ruby", src, 3, 0), Some(UsageKind::Mutate));
    }

    #[test]
    fn ruby_mixins_inherit_rescue_alias() {
        let src = "class Order < ApplicationRecord\n  include Payable\n  extend Findable\n  prepend Auditable\n  alias run call\nend\n";
        assert_eq!(kind_at("ruby", src, 1, 14), Some(UsageKind::Inherit));
        assert_eq!(kind_at("ruby", src, 2, 10), Some(UsageKind::Include));
        assert_eq!(kind_at("ruby", src, 3, 9), Some(UsageKind::Extend));
        assert_eq!(kind_at("ruby", src, 4, 10), Some(UsageKind::Prepend));
        assert_eq!(kind_at("ruby", src, 5, 8), Some(UsageKind::Alias));
        let rescued = "begin\n  run\nrescue StandardError => e\n  e\nend\n";
        assert_eq!(kind_at("ruby", rescued, 3, 7), Some(UsageKind::Rescue));
    }

    #[test]
    fn a_bare_ruby_identifier_is_unclassified_not_a_guessed_call() {
        // Ruby cannot tell a local read from a receiver-less method call
        // here; the caller supplies `read` only when it holds a binding.
        let src = "def run\n  total\nend\n";
        assert_eq!(kind_at("ruby", src, 2, 2), None);
    }

    #[test]
    fn generic_languages_get_call_and_instantiate_only() {
        assert_eq!(
            kind_at("rust", "fn a(){ b(); }\n", 1, 8),
            Some(UsageKind::Call)
        );
        assert_eq!(
            kind_at("typescript", "const x = new Foo();\n", 1, 14),
            Some(UsageKind::Instantiate)
        );
        assert_eq!(
            kind_at("typescript", "foo.bar();\n", 1, 4),
            Some(UsageKind::Call)
        );
        assert_eq!(kind_at("python", "run(1)\n", 1, 0), Some(UsageKind::Call));
        // Not a call position → no guess.
        assert_eq!(kind_at("rust", "let y = x + 1;\n", 1, 8), None);
    }

    #[test]
    fn an_unparseable_language_yields_no_kinds() {
        let out = classify_file("not-a-language", b"whatever", &[(1, 0), (1, 2)]);
        assert_eq!(out, vec![None, None]);
    }

    #[test]
    fn positions_map_one_to_one_even_when_some_miss() {
        let src = "order.total\n";
        let out = classify_file("ruby", src.as_bytes(), &[(1, 6), (99, 0), (1, 0)]);
        assert_eq!(
            out,
            vec![Some(UsageKind::Call), None, Some(UsageKind::Read)]
        );
    }
}
