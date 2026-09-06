//! PRR-N4 — ActiveRecord model macros: `association` / `scope` /
//! `callback` / `validation` / `delegate`, plus `concern_include` (shared
//! with controllers — models and controllers both `include`/`extend`
//! concerns, so [`extract_concern_includes`] is called from BOTH
//! dispatch branches in `rails::extract`, not just this file's own
//! `is_model_file` branch).
//!
//! # Scan scope: the whole class body subtree, not just top-level statements
//!
//! Unlike `routes.rs`'s DSL walk (which only recognizes TOP-LEVEL
//! statements, since route context — `module_prefix`/`default_controller`
//! — genuinely changes per nesting level), a model macro's meaning never
//! depends on which `if`/block it's nested under, so this module walks
//! `call` nodes ANYWHERE in the class body (`support::walk_calls`, the
//! same "recurse the whole subtree" idiom `views.rs::scan_calls` already
//! established for `render`/`turbo_stream.*`). Accepted, documented
//! consequence: a macro-shaped call written inside an INSTANCE method body
//! (rare, but legal Ruby) is also picked up — matching every other N4
//! extractor's posture of preferring a rare false inclusion over a missed
//! real edge.
//!
//! # Trust
//!
//! `association`: `Trust::Likely` when `class_name:` is an explicit
//! literal/constant AND the resolved file exists on disk; `Trust::
//! Candidate` when falling back to the pluralization heuristic (still
//! existence-verified — never fabricated, see the module doc's design
//! note). `scope`/`callback`/`validation` carry no cross-file dst at all,
//! so they're always `Trust::Likely` (the symbol itself IS the fact, not a
//! resolution guess). `delegate` is ALWAYS `Trust::Candidate` — the `to:`
//! target's type is never inferred, so which class actually defines the
//! delegated method is genuinely unknown (per design-addendum-2.md §G).
//! `concern_include` follows `views::make_view_edge`'s unique/ambiguous/
//! absent trust ladder.

use crate::frameworks::rails::support::{
    call_args, call_method_name, camelize_segment, collect_pairs_into, const_to_path,
    constant_text, find_pair_literal, find_pair_node, literal_string_or_symbol, singularize,
    src_line, underscore_segment, walk_calls,
};
use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::path::Path;
use tree_sitter::Node;

const CALLBACK_NAMES: &[&str] = &[
    "before_save",
    "after_save",
    "around_save",
    "before_create",
    "after_create",
    "around_create",
    "before_update",
    "after_update",
    "around_update",
    "before_destroy",
    "after_destroy",
    "around_destroy",
    "before_validation",
    "after_validation",
    "after_commit",
    "after_rollback",
    "after_initialize",
    "after_find",
];

/// `path` must already be gated to `app/models/**/*.rb`
/// (`rails::is_model_file`).
pub fn extract(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("ruby", bytes) else {
        return Vec::new();
    };
    let Some(class_node) = find_first_class_or_module(tree.root_node()) else {
        return Vec::new();
    };
    let Some(body) = class_node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_calls(body, bytes, 0, &mut out, &mut |node, source, offset| {
        resolve_model_macro(node, source, offset, path, repo_root)
    });
    out
}

fn resolve_model_macro(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    repo_root: &Path,
) -> Vec<FrameworkEdge> {
    let Some(method) = call_method_name(node, source) else {
        return Vec::new();
    };
    match method.as_str() {
        "has_many" => {
            resolve_association(node, source, line_offset, path, repo_root, true, "has_many")
        }
        "has_and_belongs_to_many" => resolve_association(
            node,
            source,
            line_offset,
            path,
            repo_root,
            true,
            "has_and_belongs_to_many",
        ),
        "has_one" => {
            resolve_association(node, source, line_offset, path, repo_root, false, "has_one")
        }
        "belongs_to" => resolve_association(
            node,
            source,
            line_offset,
            path,
            repo_root,
            false,
            "belongs_to",
        ),
        "scope" => resolve_scope(node, source, line_offset, path),
        "validates" | "validate" => resolve_validation(node, source, line_offset, path, &method),
        "delegate" => resolve_delegate(node, source, line_offset, path),
        m if CALLBACK_NAMES.contains(&m) => {
            resolve_callback(node, source, line_offset, path, &method)
        }
        _ => Vec::new(),
    }
}

/// `has_many :orders, class_name: "Order"` etc. `plural` distinguishes
/// `has_many`/`has_and_belongs_to_many` (whose bare association name is
/// plural — the default class-name guess singularizes it) from `has_one`/
/// `belongs_to` (already singular).
fn resolve_association(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    repo_root: &Path,
    plural: bool,
    macro_name: &str,
) -> Vec<FrameworkEdge> {
    let args = call_args(node);
    let mut names = Vec::new();
    let mut positional_done = false;
    let mut opts = Vec::new();
    for a in &args {
        if !positional_done {
            if let Some(name) = literal_string_or_symbol(*a, source) {
                names.push(name);
                continue;
            }
            positional_done = true;
        }
        collect_pairs_into(*a, &mut opts);
    }
    let Some(name) = names.first() else {
        return Vec::new(); // non-literal association name — never guess.
    };
    let line = src_line(node, line_offset);

    let explicit_class_name = find_pair_node(&opts, "class_name", source).and_then(|p| {
        let value = p.child_by_field_name("value")?;
        literal_string_or_symbol(value, source).or_else(|| constant_text(value, source))
    });

    let (class_name, trust) = match explicit_class_name {
        Some(cn) => (cn, Trust::Likely),
        None => {
            let guessed = if plural {
                singularize(name)
            } else {
                name.clone()
            };
            (camelize_segment(&guessed), Trust::Candidate)
        }
    };
    let dst_path = const_to_path(&class_name, "app/models");
    if !repo_root.join(&dst_path).is_file() {
        return Vec::new(); // never fabricate.
    }
    vec![FrameworkEdge {
        kind: EdgeKind::Association,
        src_path: path.to_string(),
        src_line: Some(line),
        src_symbol: Some(format!("{macro_name} :{name}")),
        dst_kind: Some("model".to_string()),
        dst_path: Some(dst_path),
        dst_symbol: None,
        trust,
        extra_json: Some(format!(r#"{{"macro":"{macro_name}"}}"#)),
    }]
}

fn resolve_scope(node: Node, source: &[u8], line_offset: u32, path: &str) -> Vec<FrameworkEdge> {
    let args = call_args(node);
    let Some(first) = args.first() else {
        return Vec::new();
    };
    let Some(name) = literal_string_or_symbol(*first, source) else {
        return Vec::new();
    };
    vec![FrameworkEdge {
        kind: EdgeKind::Scope,
        src_path: path.to_string(),
        src_line: Some(src_line(node, line_offset)),
        src_symbol: Some(name),
        dst_kind: None,
        dst_path: None,
        dst_symbol: None,
        trust: Trust::Likely,
        extra_json: None,
    }]
}

fn resolve_callback(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    callback_name: &str,
) -> Vec<FrameworkEdge> {
    let args = call_args(node);
    let line = src_line(node, line_offset);
    args.iter()
        .filter_map(|a| literal_string_or_symbol(*a, source))
        .map(|method_name| FrameworkEdge {
            kind: EdgeKind::Callback,
            src_path: path.to_string(),
            src_line: Some(line),
            src_symbol: Some(method_name),
            dst_kind: None,
            dst_path: None,
            dst_symbol: None,
            trust: Trust::Likely,
            extra_json: Some(format!(r#"{{"callback":"{callback_name}"}}"#)),
        })
        .collect()
}

fn resolve_validation(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    macro_name: &str,
) -> Vec<FrameworkEdge> {
    let args = call_args(node);
    let line = src_line(node, line_offset);
    args.iter()
        .filter_map(|a| literal_string_or_symbol(*a, source))
        .map(|attr| FrameworkEdge {
            kind: EdgeKind::Validation,
            src_path: path.to_string(),
            src_line: Some(line),
            src_symbol: Some(attr),
            dst_kind: None,
            dst_path: None,
            dst_symbol: None,
            trust: Trust::Likely,
            extra_json: Some(format!(r#"{{"macro":"{macro_name}"}}"#)),
        })
        .collect()
}

/// `delegate :a, :b, to: :target` — ALWAYS `Trust::Candidate` (the target's
/// type is never resolved, see the module doc).
fn resolve_delegate(node: Node, source: &[u8], line_offset: u32, path: &str) -> Vec<FrameworkEdge> {
    let args = call_args(node);
    let mut methods = Vec::new();
    let mut opts = Vec::new();
    for a in &args {
        if let Some(name) = literal_string_or_symbol(*a, source) {
            methods.push(name);
            continue;
        }
        collect_pairs_into(*a, &mut opts);
    }
    if methods.is_empty() {
        return Vec::new();
    }
    let to = find_pair_literal(&opts, "to", source);
    let line = src_line(node, line_offset);
    methods
        .into_iter()
        .map(|m| FrameworkEdge {
            kind: EdgeKind::Delegate,
            src_path: path.to_string(),
            src_line: Some(line),
            src_symbol: Some(m),
            dst_kind: None,
            dst_path: None,
            dst_symbol: to.clone(),
            trust: Trust::Candidate,
            extra_json: None,
        })
        .collect()
}

// --- concern_include (shared: models AND controllers) ----------------------

const CONCERN_SEARCH_ROOTS: &[&str] = &["app/models/concerns", "app/controllers/concerns"];

/// `include`/`extend SomeConcern` → `app/{models,controllers}/concerns/**/
/// <snake>.rb`. Called from BOTH `is_model_file` and `is_controller_file`
/// dispatch branches — see the module doc.
pub fn extract_concern_includes(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("ruby", bytes) else {
        return Vec::new();
    };
    let Some(class_node) = find_first_class_or_module(tree.root_node()) else {
        return Vec::new();
    };
    let Some(body) = class_node.child_by_field_name("body") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_calls(body, bytes, 0, &mut out, &mut |node, source, offset| {
        resolve_concern_include(node, source, offset, path, repo_root)
    });
    out
}

fn resolve_concern_include(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    repo_root: &Path,
) -> Vec<FrameworkEdge> {
    let Some(method) = call_method_name(node, source) else {
        return Vec::new();
    };
    if method != "include" && method != "extend" {
        return Vec::new();
    }
    let args = call_args(node);
    let Some(first) = args.first().copied() else {
        return Vec::new();
    };
    let Some(const_name) = constant_text(first, source) else {
        return Vec::new(); // non-literal (e.g. `include mod_variable`) — drop.
    };
    let matches = find_concern_files(repo_root, &const_name);
    let line = src_line(node, line_offset);
    match matches.len() {
        0 => Vec::new(),
        1 => vec![FrameworkEdge {
            kind: EdgeKind::ConcernInclude,
            src_path: path.to_string(),
            src_line: Some(line),
            src_symbol: Some(format!("{method} {const_name}")),
            dst_kind: Some("concern".to_string()),
            dst_path: Some(matches.into_iter().next().unwrap()),
            dst_symbol: None,
            trust: Trust::Likely,
            extra_json: None,
        }],
        _ => {
            let candidates = matches
                .iter()
                .map(|m| format!("\"{m}\""))
                .collect::<Vec<_>>()
                .join(",");
            vec![FrameworkEdge {
                kind: EdgeKind::ConcernInclude,
                src_path: path.to_string(),
                src_line: Some(line),
                src_symbol: Some(format!("{method} {const_name}")),
                dst_kind: Some("concern".to_string()),
                dst_path: Some(matches[0].clone()),
                dst_symbol: None,
                trust: Trust::Candidate,
                extra_json: Some(format!(r#"{{"candidates":[{candidates}]}}"#)),
            }]
        }
    }
}

fn find_concern_files(repo_root: &Path, const_name: &str) -> Vec<String> {
    let segments: Vec<String> = const_name.split("::").map(underscore_segment).collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Vec::new();
    }
    let suffix = format!("{}.rb", segments.join("/"));
    let mut out = Vec::new();
    for base in CONCERN_SEARCH_ROOTS {
        let candidate = format!("{base}/{suffix}");
        if repo_root.join(&candidate).is_file() {
            out.push(candidate);
        }
    }
    out.sort();
    out
}

/// The first `class` OR `module` node in the file (concerns are
/// `module`s, models are `class`es — one lookup covers both call sites).
fn find_first_class_or_module(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(node.kind(), "class" | "module") {
        return Some(node);
    }
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        if let Some(found) = find_first_class_or_module(c) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn explicit_class_name_literal_wins_at_likely() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/models/purchase.rb", "class Purchase; end\n");
        let src = b"class CouponUsage < ApplicationRecord\n  belongs_to :order, class_name: 'Purchase'\nend\n";
        let edges = extract(root, "app/models/coupon_usage.rb", src);
        let assoc: Vec<_> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Association)
            .collect();
        assert_eq!(assoc.len(), 1);
        assert_eq!(
            assoc[0].dst_path,
            Some("app/models/purchase.rb".to_string())
        );
        assert_eq!(assoc[0].trust, Trust::Likely);
    }

    #[test]
    fn pluralized_default_guess_is_a_verified_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/models/order.rb", "class Order; end\n");
        let src = b"class CouponUsage < ApplicationRecord\n  has_many :orders\nend\n";
        let edges = extract(root, "app/models/coupon_usage.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_path, Some("app/models/order.rb".to_string()));
        assert_eq!(edges[0].trust, Trust::Candidate);
    }

    #[test]
    fn association_with_no_matching_file_is_dropped_not_fabricated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"class CouponUsage < ApplicationRecord\n  has_many :ghosts\nend\n";
        let edges = extract(root, "app/models/coupon_usage.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn scope_is_symbol_only_no_cross_file_dst() {
        let src =
            b"class Round < ApplicationRecord\n  scope :active, -> { where(active: true) }\nend\n";
        let tmp = tempfile::tempdir().unwrap();
        let edges = extract(tmp.path(), "app/models/round.rb", src);
        let scopes: Vec<_> = edges.iter().filter(|e| e.kind == EdgeKind::Scope).collect();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].src_symbol.as_deref(), Some("active"));
        assert_eq!(scopes[0].dst_path, None);
        assert_eq!(scopes[0].trust, Trust::Likely);
    }

    #[test]
    fn callback_before_save_symbol_resolves() {
        let src = b"class Round < ApplicationRecord\n  before_save :normalize!\nend\n";
        let tmp = tempfile::tempdir().unwrap();
        let edges = extract(tmp.path(), "app/models/round.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::Callback);
        assert_eq!(edges[0].src_symbol.as_deref(), Some("normalize!"));
    }

    #[test]
    fn callback_with_a_block_body_has_no_symbol_and_emits_nothing() {
        let src =
            b"class BundleItem < ApplicationRecord\n  before_create { raise Foo if bar? }\nend\n";
        let tmp = tempfile::tempdir().unwrap();
        let edges = extract(tmp.path(), "app/models/bundle_item.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn validates_multiple_attributes_each_get_their_own_edge() {
        let src =
            b"class Order < ApplicationRecord\n  validates :state, :status, presence: true\nend\n";
        let tmp = tempfile::tempdir().unwrap();
        let edges = extract(tmp.path(), "app/models/order.rb", src);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|e| e.kind == EdgeKind::Validation));
        let syms: Vec<_> = edges.iter().filter_map(|e| e.src_symbol.clone()).collect();
        assert!(syms.contains(&"state".to_string()));
        assert!(syms.contains(&"status".to_string()));
    }

    #[test]
    fn delegate_is_always_candidate_with_unknown_target_type() {
        let src =
            b"class Store < ApplicationRecord\n  delegate :next_day_count, to: :purchases\nend\n";
        let tmp = tempfile::tempdir().unwrap();
        let edges = extract(tmp.path(), "app/models/store.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::Delegate);
        assert_eq!(edges[0].trust, Trust::Candidate);
        assert_eq!(edges[0].dst_symbol.as_deref(), Some("purchases"));
    }

    #[test]
    fn concern_include_resolves_unique_match() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/models/concerns/discountable.rb",
            "module Discountable; end\n",
        );
        let src = b"class Order < ApplicationRecord\n  include Discountable\nend\n";
        let edges = extract_concern_includes(root, "app/models/order.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/models/concerns/discountable.rb".to_string())
        );
        assert_eq!(edges[0].trust, Trust::Likely);
    }

    #[test]
    fn concern_include_namespaced_constant_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/models/concerns/trade/discountable.rb",
            "module Trade; module Discountable; end; end\n",
        );
        let src = b"class Round < ApplicationRecord\n  include Trade::Discountable\nend\n";
        let edges = extract_concern_includes(root, "app/models/round.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/models/concerns/trade/discountable.rb".to_string())
        );
    }

    #[test]
    fn concern_include_non_literal_is_dropped() {
        let src = b"class Order < ApplicationRecord\n  include some_module_variable\nend\n";
        let tmp = tempfile::tempdir().unwrap();
        let edges = extract_concern_includes(tmp.path(), "app/models/order.rb", src);
        assert!(edges.is_empty());
    }
}
