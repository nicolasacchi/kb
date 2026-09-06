//! V71-E1 — the Ruby locals lane for `usages/2`, under D4's STRICT rule.
//!
//! Ruby is the adversarial case for references: `send`, `define_method`,
//! `method_missing`, `instance_eval` and `binding` can all make a name that
//! *looks* like a local into something else at runtime, and the static
//! ladder (`crate::resolve`) has no `exact` tier for it at all — the four
//! `locals::supports` languages are Rust/TS/TSX/Python. D4 opens exactly
//! one door, and closes it three ways:
//!
//! > a locals query mints **exact** only when an unambiguous binding site
//! > exists in the same lexical scope AND the name is not a known method in
//! > the enclosing hierarchy AND the enclosing method contains no
//! > `eval`/`instance_eval`/`binding`/`send`/`define_method`/
//! > `method_missing`; otherwise **likely**.
//!
//! Those three clauses are [`StrictVerdict`]'s three refusal reasons. This
//! module owns clauses (a) and (c) plus the *evidence* for (b) (the
//! enclosing constant hierarchy, including `include`/`prepend`/`extend`);
//! the caller supplies (b)'s answer, because deciding "is this name a
//! method somewhere in that hierarchy" needs the store and this module is
//! pure (the `intel::*` house rule).
//!
//! # Why read-time, not ingest-time
//!
//! Nothing here is persisted and nothing here bumps the Ruby salt. The
//! binding is derived from the file's own bytes on every request, which is
//! both root CLAUDE.md invariant #2's posture ("kb-code mints CLASSES …
//! computed per request and NEVER persisted") and the cheap way in: adding
//! Ruby to `locals::supports` would stamp `local_def_ordinal` at ingest and
//! owe a re-extract of the whole 6.5K-file target corpus.
//!
//! # What a binding GROUP is (and the site it under-reports)
//!
//! A group is keyed on the binding a reference actually resolves to, which
//! for a reassigned local is the MOST RECENT assignment before that
//! reference. `x = nil; …; x = 2; …; use(x)` therefore yields a group of
//! `{the L2 assignment, the use}` — the FIRST assignment is not a site of
//! it. That is deliberate: grouping every same-name assignment in a scope
//! together would also merge a block-local shadow with the outer variable,
//! which is the one thing clause (a) exists to notice. Nothing is lost
//! from the answer — the earlier assignment is still an ordinary same-file
//! name match at `likely` — it is only not claimed as `exact`. Pinned by
//! the `apply_coupon_service.rb` oracle case.
//!
//! # The refusal direction
//!
//! Every uncertainty in this module resolves DOWNWARD — an unparseable
//! file, a reference the graph cannot bind, a name it can bind two ways —
//! yields `likely`, never `exact`, and never an empty answer where a row
//! existed. A wrong `exact` is a release blocker; a missing one is a
//! smaller, visible loss.

/// The dynamic constructs D4 names, verbatim. A method body containing any
/// of them (as a called method name or a bare identifier) can rewrite what
/// a name means at runtime, so no `exact` may be minted for a local
/// declared inside it.
///
/// The list is deliberately D4's, unextended: `class_eval`/`module_eval`/
/// `public_send`/`constantize` are NOT here. That is a recorded gap, not an
/// oversight — widening it is a ruling to take with the oracle set in hand,
/// since every addition demotes real rows.
pub const DYNAMIC_CONSTRUCTS: &[&str] = &[
    "eval",
    "instance_eval",
    "binding",
    "send",
    "define_method",
    "method_missing",
];

/// Why the STRICT rule refused (or did not) for one binding group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrictVerdict {
    /// All three clauses hold — this group may be minted `exact`.
    Exact,
    /// (a) more than one scope in the lookup chain binds this name.
    AmbiguousBinding,
    /// (b) the name is also a method in the enclosing constant hierarchy.
    HierarchyMethod,
    /// (c) the enclosing method uses one of [`DYNAMIC_CONSTRUCTS`].
    DynamicEnclosing,
}

impl StrictVerdict {
    pub fn is_exact(self) -> bool {
        matches!(self, StrictVerdict::Exact)
    }

    /// Short, stable reason string — surfaced on the wire so a reader can
    /// see WHY a Ruby row is `likely` rather than guessing.
    pub fn as_str(self) -> &'static str {
        match self {
            StrictVerdict::Exact => "strict",
            StrictVerdict::AmbiguousBinding => "ambiguous-binding",
            StrictVerdict::HierarchyMethod => "hierarchy-method",
            StrictVerdict::DynamicEnclosing => "dynamic-enclosing",
        }
    }
}

/// One same-file site of the queried local: the binding site itself or a
/// reference bound to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSite {
    /// 1-based line.
    pub line: u32,
    /// 0-based byte column.
    pub col: u32,
    /// `true` for the binding site (the assignment LHS / parameter).
    pub is_def: bool,
}

/// The result of the Ruby locals lane at one position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalsLane {
    pub name: String,
    /// Every site, sorted by `(line, col)`, deduped.
    pub sites: Vec<LocalSite>,
    pub verdict: StrictVerdict,
    /// The enclosing constant hierarchy that clause (b) was evaluated
    /// against (outermost first), e.g. `["Billing::Invoice",
    /// "ApplicationRecord", "Concerns::Payable"]`. Empty at file scope.
    pub hierarchy: Vec<String>,
}

/// The enclosing constant hierarchy at `byte` — the `class`/`module` chain
/// that lexically contains it, each one's `superclass`, and every
/// `include`/`prepend`/`extend` argument constant in those bodies
/// (D4: "hierarchy incl. include/prepend/extend").
///
/// Names are the source text of the constant node, so `Concerns::Payable`
/// stays qualified and `ApplicationRecord` stays bare — matching what
/// `extract::Symbol::container` stores, which is what the caller compares
/// against. Nothing is resolved to a file: this is a NAME set, and the
/// caller's lookup over it is a `likely`-grade join by construction (which
/// is fine — it only ever demotes).
pub fn enclosing_hierarchy(source: &[u8], byte: usize) -> Vec<String> {
    let Ok((tree, _)) = crate::lang::parse("ruby", source) else {
        return Vec::new();
    };
    let root = tree.root_node();
    let Some(mut node) = root.descendant_for_byte_range(byte, byte) else {
        return Vec::new();
    };
    let mut chain: Vec<tree_sitter::Node<'_>> = Vec::new();
    loop {
        if matches!(node.kind(), "class" | "module") {
            chain.push(node);
        }
        match node.parent() {
            Some(p) => node = p,
            None => break,
        }
    }
    chain.reverse(); // outermost first
    let mut out: Vec<String> = Vec::new();
    let push = |s: &str, out: &mut Vec<String>| {
        let s = s.trim();
        if !s.is_empty() && !out.iter().any(|e| e == s) {
            out.push(s.to_string());
        }
    };
    for n in &chain {
        if let Some(name) = n.child_by_field_name("name") {
            if let Ok(t) = name.utf8_text(source) {
                push(t, &mut out);
            }
        }
        if let Some(sup) = n.child_by_field_name("superclass") {
            // `(superclass (constant))` — take the expression's text.
            let mut c = sup.walk();
            for child in sup.named_children(&mut c) {
                if let Ok(t) = child.utf8_text(source) {
                    push(t, &mut out);
                }
            }
        }
        if let Some(body) = n.child_by_field_name("body") {
            for name in mixin_constants(body, source) {
                push(&name, &mut out);
            }
        }
    }
    out
}

/// Every constant named by a DIRECT `include`/`prepend`/`extend` call in a
/// class/module body. Direct only: a mixin applied inside a nested method,
/// a `concerning` block or a metaprogrammed loop is not read, because the
/// name would not be provable from this node.
fn mixin_constants(body: tree_sitter::Node<'_>, source: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cursor = body.walk();
    for stmt in body.named_children(&mut cursor) {
        if stmt.kind() != "call" {
            continue;
        }
        if stmt.child_by_field_name("receiver").is_some() {
            continue;
        }
        let Some(method) = stmt.child_by_field_name("method") else {
            continue;
        };
        let Ok(mname) = method.utf8_text(source) else {
            continue;
        };
        if !matches!(mname, "include" | "prepend" | "extend") {
            continue;
        }
        let Some(args) = stmt.child_by_field_name("arguments") else {
            continue;
        };
        let mut ac = args.walk();
        for arg in args.named_children(&mut ac) {
            if matches!(arg.kind(), "constant" | "scope_resolution") {
                if let Ok(t) = arg.utf8_text(source) {
                    out.push(t.to_string());
                }
            }
        }
    }
    out
}

/// `true` when the method (or, at file scope, the whole file) enclosing
/// `byte` names one of [`DYNAMIC_CONSTRUCTS`] — clause (c).
///
/// Walks the CST rather than the raw text so a construct named only in a
/// comment or a string does not demote a whole method. The scan covers the
/// enclosing `method`/`singleton_method` node; with none (a top-level
/// script, a `class << self` body) it falls back to the enclosing barrier
/// construct, and finally to the whole file — always the LARGER region,
/// never a smaller one, so the answer can only get stricter.
pub fn encloses_dynamic_construct(source: &[u8], byte: usize) -> bool {
    let Ok((tree, _)) = crate::lang::parse("ruby", source) else {
        // Unparseable: refuse (cannot prove the absence of anything).
        return true;
    };
    let root = tree.root_node();
    let scan_root = root
        .descendant_for_byte_range(byte, byte)
        .map(|n| enclosing_region(n))
        .unwrap_or(root);
    node_names_dynamic_construct(scan_root, source)
}

/// The smallest `method`/`singleton_method` containing `node`, else the
/// smallest `class`/`module`/`singleton_class`, else the root.
fn enclosing_region(node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    let mut cur = Some(node);
    let mut fallback = None;
    while let Some(n) = cur {
        match n.kind() {
            "method" | "singleton_method" => return n,
            "class" | "module" | "singleton_class" if fallback.is_none() => fallback = Some(n),
            _ => {}
        }
        cur = n.parent();
    }
    fallback.unwrap_or(node)
}

fn node_names_dynamic_construct(node: tree_sitter::Node<'_>, source: &[u8]) -> bool {
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "identifier" | "constant") {
            if let Ok(t) = n.utf8_text(source) {
                if DYNAMIC_CONSTRUCTS.contains(&t) {
                    return true;
                }
            }
        }
        for child in n.named_children(&mut cursor) {
            stack.push(child);
        }
    }
    false
}

/// Bind the Ruby local at `(line, col)` and evaluate D4's STRICT rule.
///
/// `hierarchy_method` answers clause (b) for a `(name, hierarchy)` pair —
/// the caller's store lookup ("is `name` a method defined on any of these
/// constants anywhere in the repo?"). It is called at most once.
///
/// `None` when the position is not a Ruby local at all (an unbound
/// identifier, a constant, a method call with no binding site) — the caller
/// then falls through to the ordinary name-match ladder, which is exactly
/// the pre-V71 behaviour.
pub fn locals_lane(
    source: &[u8],
    line: u32,
    col: u32,
    hierarchy_method: impl FnOnce(&str, &[String]) -> bool,
) -> Option<LocalsLane> {
    let bindings = crate::locals::bind_locals("ruby", source).ok()?;
    if bindings.is_empty() {
        return None;
    }
    let point_byte = byte_offset_of(source, line, col)?;

    // Which binding group does the position belong to? Either it IS a
    // reference (its span covers the point) or it IS the binding site
    // (a def span covers the point).
    let group_def: (usize, usize) = bindings
        .iter()
        .find(|b| b.ref_start_byte <= point_byte && point_byte < b.ref_end_byte)
        .map(|b| (b.def_start_byte, b.def_end_byte))
        .or_else(|| {
            bindings
                .iter()
                .find(|b| b.def_start_byte <= point_byte && point_byte < b.def_end_byte)
                .map(|b| (b.def_start_byte, b.def_end_byte))
        })?;

    let group: Vec<&crate::locals::Binding> = bindings
        .iter()
        .filter(|b| (b.def_start_byte, b.def_end_byte) == group_def)
        .collect();
    let first = group.first()?;
    let name = first.name.clone();

    let mut sites: Vec<LocalSite> = vec![LocalSite {
        line: first.def_line,
        col: first.def_col,
        is_def: true,
    }];
    for b in &group {
        sites.push(LocalSite {
            line: b.ref_line,
            col: b.ref_col,
            is_def: false,
        });
    }
    sites.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.col.cmp(&b.col)));
    sites.dedup_by(|a, b| a.line == b.line && a.col == b.col);

    // (a) unambiguous binding site in the same lexical scope.
    let ambiguous = group.iter().any(|b| b.visible_defs > 1);
    let hierarchy = enclosing_hierarchy(source, group_def.0);
    let verdict = if ambiguous {
        StrictVerdict::AmbiguousBinding
    } else if encloses_dynamic_construct(source, group_def.0) {
        // (c) before (b): (c) is answered from this file alone, (b) costs
        // the caller a store query.
        StrictVerdict::DynamicEnclosing
    } else if hierarchy_method(&name, &hierarchy) {
        StrictVerdict::HierarchyMethod
    } else {
        StrictVerdict::Exact
    };

    Some(LocalsLane {
        name,
        sites,
        verdict,
        hierarchy,
    })
}

/// Byte offset of a 1-based `line` / 0-based byte `col`. `None` when the
/// position is past the end of the file.
fn byte_offset_of(source: &[u8], line: u32, col: u32) -> Option<usize> {
    if line == 0 {
        return None;
    }
    let mut cur_line = 1u32;
    let mut idx = 0usize;
    while cur_line < line {
        let nl = source[idx..].iter().position(|&b| b == b'\n')?;
        idx += nl + 1;
        cur_line += 1;
    }
    let off = idx + col as usize;
    (off <= source.len()).then_some(off)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(src: &str, line: u32, col: u32) -> Option<LocalsLane> {
        locals_lane(src.as_bytes(), line, col, |_, _| false)
    }

    #[test]
    fn a_plain_method_local_is_strict_exact() {
        let src = "class Order\n  def total\n    subtotal = 1\n    subtotal + 2\n  end\nend\n";
        let l = lane(src, 4, 4).expect("bound");
        assert_eq!(l.name, "subtotal");
        assert_eq!(l.verdict, StrictVerdict::Exact);
        assert_eq!(l.sites.len(), 2, "def + one ref: {:?}", l.sites);
        assert!(l.sites[0].is_def);
    }

    #[test]
    fn a_method_body_never_binds_a_top_level_local() {
        // The barrier: without it, `total` inside `def show` would bind to
        // the script-level `total` and mint a wrong exact.
        let src = "total = 1\ndef show\n  total\nend\n";
        assert!(
            lane(src, 3, 2).is_none(),
            "a method body must not see a script-level local"
        );
    }

    #[test]
    fn a_block_does_see_the_enclosing_method_local() {
        let src = "def run\n  base = 1\n  [1].each { |n| base + n }\nend\n";
        let l = lane(src, 3, 17).expect("blocks are closures");
        assert_eq!(l.name, "base");
        assert_eq!(l.verdict, StrictVerdict::Exact);
    }

    #[test]
    fn a_shadowing_block_binding_is_ambiguous_not_exact() {
        let src = "def run\n  row = 1\n  [1].each do |x|\n    row = x\n    row + 1\n  end\nend\n";
        let l = lane(src, 5, 4).expect("bound");
        assert_eq!(
            l.verdict,
            StrictVerdict::AmbiguousBinding,
            "two scopes bind `row`: {:?}",
            l.sites
        );
    }

    #[test]
    fn reassignment_in_one_scope_is_still_unambiguous() {
        let src = "def run\n  value = nil\n  value = 2\n  value + 1\nend\n";
        let l = lane(src, 4, 2).expect("bound");
        assert_eq!(l.verdict, StrictVerdict::Exact);
    }

    #[test]
    fn send_anywhere_in_the_enclosing_method_demotes() {
        let src = "def check\n  fields = [:a]\n  fields.all? { |f| obj.send(f) }\nend\n";
        let l = lane(src, 3, 2).expect("bound");
        assert_eq!(l.verdict, StrictVerdict::DynamicEnclosing);
    }

    #[test]
    fn each_dynamic_construct_demotes_and_a_lookalike_does_not() {
        for c in DYNAMIC_CONSTRUCTS {
            let src = format!("def run\n  v = 1\n  {c}\n  v\nend\n");
            let l = lane(&src, 4, 2).expect("bound");
            assert_eq!(l.verdict, StrictVerdict::DynamicEnclosing, "{c}");
        }
        // `public_send`/`class_eval` are NOT on D4's list — recorded gap.
        let src = "def run\n  v = 1\n  public_send(:x)\n  v\nend\n";
        assert_eq!(lane(src, 4, 2).unwrap().verdict, StrictVerdict::Exact);
    }

    #[test]
    fn a_dynamic_construct_named_only_in_a_comment_or_string_does_not_demote() {
        let src = "def run\n  v = 1 # no send here\n  s = \"send\"\n  v\nend\n";
        assert_eq!(lane(src, 4, 2).unwrap().verdict, StrictVerdict::Exact);
    }

    #[test]
    fn a_dynamic_construct_in_a_sibling_method_does_not_demote() {
        let src = "def a\n  obj.send(:x)\nend\n\ndef b\n  v = 1\n  v\nend\n";
        assert_eq!(lane(src, 7, 2).unwrap().verdict, StrictVerdict::Exact);
    }

    #[test]
    fn the_hierarchy_clause_is_the_callers_answer() {
        let src = "class Order < ApplicationRecord\n  include Payable\n  def run\n    status = 1\n    status\n  end\nend\n";
        let seen = std::cell::RefCell::new(Vec::new());
        let l = locals_lane(src.as_bytes(), 5, 4, |name, hier| {
            seen.borrow_mut().push((name.to_string(), hier.to_vec()));
            true
        })
        .expect("bound");
        assert_eq!(l.verdict, StrictVerdict::HierarchyMethod);
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 1, "one lookup only");
        assert_eq!(seen[0].0, "status");
        assert_eq!(
            seen[0].1,
            vec![
                "Order".to_string(),
                "ApplicationRecord".to_string(),
                "Payable".to_string()
            ]
        );
    }

    #[test]
    fn hierarchy_reads_class_module_superclass_and_every_mixin_form() {
        let src = "module Billing\n  class Invoice < ApplicationRecord\n    include Concerns::Payable\n    prepend Auditable\n    extend Findable\n    def run\n      v = 1\n      v\n    end\n  end\nend\n";
        let l = lane(src, 8, 6).expect("bound");
        assert_eq!(
            l.hierarchy,
            vec![
                "Billing",
                "Invoice",
                "ApplicationRecord",
                "Concerns::Payable",
                "Auditable",
                "Findable"
            ]
        );
    }

    #[test]
    fn a_constant_or_method_call_is_not_a_local_lane() {
        let src = "class Order\n  def run\n    Payment.capture\n  end\nend\n";
        assert!(lane(src, 3, 4).is_none());
    }

    #[test]
    fn an_unparseable_position_never_yields_exact() {
        // Past EOF: no lane rather than a guess.
        let src = "def run\n  v = 1\n  v\nend\n";
        assert!(lane(src, 99, 0).is_none());
    }

    #[test]
    fn a_rescue_variable_is_a_binding_site() {
        let src = "def run\n  risky\nrescue StandardError => e\n  report(e)\nend\n";
        let l = lane(src, 4, 9).expect("bound");
        assert_eq!(l.name, "e");
        assert_eq!(l.verdict, StrictVerdict::Exact);
        assert!(l.sites.iter().any(|s| s.is_def && s.line == 3));
    }
}
