//! V3.G1 — lexical scope graph + same-file binding resolution over a
//! tree-sitter `locals.scm` query.
//!
//! Given a parse tree and the language's locals query (`lang::locals_query`),
//! this module:
//! 1. collects `@local.scope`, `@local.definition*` and `@local.reference`
//!    captures;
//! 2. builds a nestable scope tree (innermost-scope-first);
//! 3. places each definition in its enclosing scope;
//! 4. binds each reference to the nearest enclosing definition of the same
//!    text, honouring language visibility rules (see [`DefVisibility`]).
//!
//! Pure functions only — no store, no I/O. The occurrences pass
//! (`occurrences::extract_occurrences`) calls [`bind_locals`] and stamps
//! `Occurrence::local_def_ordinal` from the resulting [`Binding`]s.
//!
//! # Trust posture
//!
//! This is TRUST-CRITICAL: a wrong exact-class result is a release blocker.
//! When a case is ambiguous (TDZ subtleties, unusual patterns, incomplete
//! query coverage), we leave the reference **unbound** so the resolve
//! ladder falls through to `file-local`/`import-heuristic`/`tags-approx`
//! (likely/candidate) rather than claiming `exact`.

use crate::lang::{self, LangError};
use std::collections::HashMap;
use tree_sitter::StreamingIterator;

pub type Result<T> = std::result::Result<T, LangError>;

/// Languages with a vendored `*-locals.scm` (V3.G1 proof set — D4).
pub const LOCALS_LANG_IDS: &[&str] = &["rust", "typescript", "tsx", "python"];

pub fn supports(lang_id: &str) -> bool {
    LOCALS_LANG_IDS.contains(&lang_id)
}

/// How a definition is visible inside its enclosing scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefVisibility {
    /// Visible throughout the enclosing scope (function/class decls that
    /// hoist; parameters; Python assignments — no TDZ).
    EntireScope,
    /// Visible only after the enclosing declaration node ends (Rust `let`,
    /// TS/JS `let`/`const`/`var`). Prevents `let a = a` from self-binding.
    AfterDef,
}

/// One resolved same-file reference → definition binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub ref_start_byte: usize,
    pub ref_end_byte: usize,
    pub ref_line: u32,
    pub ref_col: u32,
    pub def_start_byte: usize,
    pub def_end_byte: usize,
    pub def_line: u32,
    pub def_col: u32,
    /// V71-E1 — how many SCOPES in the lookup chain (innermost outward, up
    /// to and including the first barrier) hold a visible definition of
    /// this name. `1` is the unambiguous case D4's Ruby STRICT rule
    /// requires; `> 1` means a shadowing chain (an inner block binding over
    /// an outer method local, say) where the binding a reader would assume
    /// and the binding the graph picked can legitimately differ.
    ///
    /// Reassignment inside ONE scope (`x = 1; …; x = 2`) is one variable
    /// and counts once — the axis here is shadowing across scopes, not
    /// assignment count.
    pub visible_defs: usize,
}

#[derive(Debug, Clone)]
struct Scope {
    start: usize,
    end: usize,
    parent: Option<usize>,
    defs: Vec<Def>,
    /// V71-E1 — a `@local.scope.isolated` capture: name lookup that
    /// reaches this scope STOPS here instead of walking outward. Ruby
    /// method/class/module bodies do not close over the enclosing script's
    /// locals (upstream tree-sitter-ruby says so with
    /// `(#set! local.scope-inherits false)`, which this module carries as
    /// a capture name since it evaluates no query predicates). Every
    /// pre-V71 language captures only `@local.scope`, so their scope trees
    /// are byte-identical to before.
    barrier: bool,
}

#[derive(Debug, Clone)]
struct Def {
    name: String,
    start: usize,
    end: usize,
    line: u32,
    col: u32,
    /// Byte offset at which this def becomes visible (AfterDef: end of
    /// enclosing declaration; EntireScope: 0).
    visible_from: usize,
    visibility: DefVisibility,
}

#[derive(Debug, Clone)]
struct RawDef {
    name: String,
    start: usize,
    end: usize,
    line: u32,
    col: u32,
    visibility: DefVisibility,
    /// End of the declaration that owns this binding (for AfterDef).
    decl_end: usize,
    /// When true, place this def in the parent of the innermost enclosing
    /// scope (function/class names — see placement loop below).
    hoist_to_parent: bool,
}

#[derive(Debug, Clone)]
struct RawRef {
    name: String,
    start: usize,
    end: usize,
    line: u32,
    col: u32,
}

/// Build same-file bindings for `source` parsed as `lang_id`.
///
/// Returns an empty vec (not an error) when `lang_id` has no locals query
/// — callers that care should gate on [`supports`] first. `Err` only for
/// parse/query failures on a supported language.
pub fn bind_locals(lang_id: &str, source: &[u8]) -> Result<Vec<Binding>> {
    let Some(query_src) = lang::locals_query(lang_id) else {
        return Ok(Vec::new());
    };
    let (tree, language) = lang::parse(lang_id, source)?;
    let query = lang::compile_query(lang_id, &language, query_src)?;
    let capture_names = query.capture_names();

    let mut scope_ranges: Vec<(usize, usize, bool)> = Vec::new();
    let mut raw_defs: Vec<RawDef> = Vec::new();
    let mut raw_refs: Vec<RawRef> = Vec::new();
    // Byte ranges of every definition identifier — a capture that is both
    // a definition and a reference (the query often fires both on the same
    // node) is treated as a def only.
    let mut def_spans: HashMap<(usize, usize), ()> = HashMap::new();

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source);

    while let Some(m) = matches.next() {
        for cap in m.captures {
            let cname = capture_names[cap.index as usize];
            let node = cap.node;
            if cname == "local.scope" || cname == "local.scope.isolated" {
                scope_ranges.push((
                    node.start_byte(),
                    node.end_byte(),
                    cname == "local.scope.isolated",
                ));
                continue;
            }
            if let Some(suffix) = definition_suffix(cname) {
                let name = match node.utf8_text(source) {
                    Ok(s) if !s.is_empty() => s.to_string(),
                    _ => continue,
                };
                let start = node.start_position();
                let visibility = visibility_for_suffix(suffix);
                let decl_end = declaration_end_for(lang_id, node, visibility);
                def_spans.insert((node.start_byte(), node.end_byte()), ());
                raw_defs.push(RawDef {
                    name,
                    start: node.start_byte(),
                    end: node.end_byte(),
                    line: start.row as u32 + 1,
                    col: start.column as u32,
                    visibility,
                    decl_end,
                    hoist_to_parent: matches!(suffix, "function" | "class"),
                });
                continue;
            }
            if cname == "local.reference" {
                // Member/attribute *field* names are not lexical refs
                // (Python `self.name`'s `name`, etc.). Binding them to a
                // same-text local would mint a wrong exact. Only the
                // object/receiver side is a name use.
                if is_member_field_name(node) {
                    continue;
                }
                let name = match node.utf8_text(source) {
                    Ok(s) if !s.is_empty() => s.to_string(),
                    _ => continue,
                };
                let start = node.start_position();
                raw_refs.push(RawRef {
                    name,
                    start: node.start_byte(),
                    end: node.end_byte(),
                    line: start.row as u32 + 1,
                    col: start.column as u32,
                });
            }
        }
    }

    // Ensure a file-wide root scope so top-level defs always have a home.
    let root_end = tree.root_node().end_byte();
    if !scope_ranges
        .iter()
        .any(|&(s, e, _)| s == 0 && e >= root_end)
    {
        scope_ranges.push((0, root_end.max(1), false));
    }

    let mut scopes = build_scope_tree(&scope_ranges);

    // Place each def in its enclosing scope. Function/class *names* sit
    // inside their own function_item/class scope node; those defs must be
    // hoisted to the PARENT scope so sibling call sites can see them
    // (standard locals.scm semantics: the name binds in the enclosing
    // block/module, the body is a nested scope).
    for d in &raw_defs {
        let Some(idx) = innermost_scope(&scopes, d.start, d.end) else {
            continue;
        };
        let place = if d.hoist_to_parent {
            scopes[idx].parent.unwrap_or(idx)
        } else {
            idx
        };
        let visible_from = match d.visibility {
            DefVisibility::EntireScope => 0,
            DefVisibility::AfterDef => d.decl_end,
        };
        scopes[place].defs.push(Def {
            name: d.name.clone(),
            start: d.start,
            end: d.end,
            line: d.line,
            col: d.col,
            visible_from,
            visibility: d.visibility,
        });
    }

    // Resolve each reference (skip pure def sites).
    let mut bindings = Vec::new();
    for r in &raw_refs {
        if def_spans.contains_key(&(r.start, r.end)) {
            continue;
        }
        let Some(start_scope) = innermost_scope_point(&scopes, r.start) else {
            continue;
        };
        if let Some(def) = lookup(&scopes, start_scope, &r.name, r.start) {
            let visible_defs = count_visible_scopes(&scopes, start_scope, &r.name, r.start);
            bindings.push(Binding {
                name: r.name.clone(),
                ref_start_byte: r.start,
                ref_end_byte: r.end,
                ref_line: r.line,
                ref_col: r.col,
                def_start_byte: def.start,
                def_end_byte: def.end,
                def_line: def.line,
                def_col: def.col,
                visible_defs,
            });
        }
    }
    Ok(bindings)
}

fn definition_suffix(cname: &str) -> Option<&str> {
    if cname == "local.definition" {
        return Some("");
    }
    cname.strip_prefix("local.definition.")
}

/// `true` when `node` is the field/property side of a member access — not a
/// lexical name use. Python's grammar reuses `identifier` for both
/// `obj` and `attr` in `obj.attr`; TS/Rust usually use a distinct leaf kind
/// for the field, so this is mostly a Python safety net (and a cheap
/// no-op elsewhere).
fn is_member_field_name(node: tree_sitter::Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        // Python: (attribute object: … attribute: (identifier))
        "attribute" => parent.child_by_field_name("attribute") == Some(node),
        // Defensive: if a JS/TS grammar ever puts a bare identifier in the
        // property field (normally `property_identifier`).
        "member_expression" => parent.child_by_field_name("property") == Some(node),
        // Rust field access uses `field_identifier` (not captured as a
        // local.reference); keep the check for symmetry.
        "field_expression" => parent.child_by_field_name("field") == Some(node),
        _ => false,
    }
}

fn visibility_for_suffix(suffix: &str) -> DefVisibility {
    match suffix {
        // Sequential bindings (Rust let, TS/JS let/const/var).
        "var" => DefVisibility::AfterDef,
        // Function/class names, parameters, Python assignments
        // (`.variable`), bare `@local.definition`.
        "function" | "class" | "parameter" | "variable" | "" => DefVisibility::EntireScope,
        // Unknown subtype — classify DOWN (sequential is safer than
        // claiming whole-scope visibility for something we don't know).
        _ => DefVisibility::AfterDef,
    }
}

/// End of the declaration that owns `node`, used as AfterDef visibility
/// start. Walks up a short ancestor chain looking for a known declaration
/// form; falls back to the identifier's own end (conservative: may
/// over-bind less often than under-bind for `let a = a`).
fn declaration_end_for(
    lang_id: &str,
    node: tree_sitter::Node<'_>,
    visibility: DefVisibility,
) -> usize {
    if visibility != DefVisibility::AfterDef {
        return node.end_byte();
    }
    let decl_kinds: &[&str] = match lang_id {
        "rust" => &[
            "let_declaration",
            "for_expression",
            "match_arm",
            "match_pattern",
            "if_let_expression",
            "while_let_expression",
            "const_item",
            "static_item",
        ],
        "typescript" | "tsx" | "javascript" => &[
            "variable_declarator",
            "lexical_declaration",
            "variable_declaration",
            "for_in_statement",
            "for_of_statement",
            "for_statement",
            "catch_clause",
        ],
        "python" => &[
            "assignment",
            "for_statement",
            "for_in_clause",
            "except_clause",
            "with_item",
        ],
        // V71-E1 — Ruby's assignment forms. `exception_variable` is the
        // `rescue … => e` binding; it has no separate declaration node, so
        // the identifier's own end is the right visibility start.
        "ruby" => &[
            "assignment",
            "operator_assignment",
            "left_assignment_list",
            "destructured_left_assignment",
            "rest_assignment",
        ],
        _ => &[],
    };
    let mut cur = node;
    for _ in 0..8 {
        if let Some(parent) = cur.parent() {
            if decl_kinds.contains(&parent.kind()) {
                return parent.end_byte();
            }
            cur = parent;
        } else {
            break;
        }
    }
    node.end_byte()
}

fn build_scope_tree(ranges: &[(usize, usize, bool)]) -> Vec<Scope> {
    // Dedup identical ranges, sort by start ASC then end DESC so a parent
    // (wider) appears before its children when starts equal. Two captures
    // over the SAME range with different barrier flags collapse to a
    // barrier (classify DOWN: refusing to walk outward can only ever cost
    // an exact, never mint a wrong one).
    let mut uniq: Vec<(usize, usize, bool)> = ranges.to_vec();
    uniq.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
    uniq.dedup_by(|a, b| {
        if a.0 == b.0 && a.1 == b.1 {
            b.2 = b.2 || a.2;
            true
        } else {
            false
        }
    });

    let mut scopes: Vec<Scope> = uniq
        .iter()
        .map(|&(start, end, barrier)| Scope {
            start,
            end,
            parent: None,
            defs: Vec::new(),
            barrier,
        })
        .collect();

    // Parent = narrowest earlier scope that strictly contains this one.
    for i in 0..scopes.len() {
        let (s, e) = (scopes[i].start, scopes[i].end);
        let mut parent: Option<usize> = None;
        let mut parent_span = usize::MAX;
        for (j, other) in scopes.iter().enumerate() {
            if i == j {
                continue;
            }
            let (ps, pe) = (other.start, other.end);
            // Strict containment.
            if ps <= s && e <= pe && (ps < s || e < pe) {
                let span = pe - ps;
                if span < parent_span {
                    parent_span = span;
                    parent = Some(j);
                }
            }
        }
        scopes[i].parent = parent;
    }
    scopes
}

fn innermost_scope(scopes: &[Scope], start: usize, end: usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_span = usize::MAX;
    for (i, sc) in scopes.iter().enumerate() {
        if sc.start <= start && end <= sc.end {
            let span = sc.end - sc.start;
            if span < best_span {
                best_span = span;
                best = Some(i);
            }
        }
    }
    best
}

fn innermost_scope_point(scopes: &[Scope], point: usize) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_span = usize::MAX;
    for (i, sc) in scopes.iter().enumerate() {
        if sc.start <= point && point < sc.end {
            let span = sc.end - sc.start;
            if span < best_span {
                best_span = span;
                best = Some(i);
            }
        }
    }
    best
}

/// Walk scopes outward from `scope_idx` looking for `name` visible at
/// `ref_start`. Within one scope, the most recent visible def wins
/// (shadowing). The walk STOPS after a [`Scope::barrier`] scope — that
/// scope is searched, its ancestors are not (V71-E1; Ruby method/class
/// bodies).
fn lookup<'a>(
    scopes: &'a [Scope],
    scope_idx: usize,
    name: &str,
    ref_start: usize,
) -> Option<&'a Def> {
    let mut idx = Some(scope_idx);
    while let Some(i) = idx {
        let mut best: Option<&Def> = None;
        for d in &scopes[i].defs {
            if d.name != name {
                continue;
            }
            if !def_visible(d, ref_start) {
                continue;
            }
            // Prefer the rightmost (most recent) def in this scope.
            match best {
                None => best = Some(d),
                Some(prev) if d.start >= prev.start => best = Some(d),
                Some(_) => {}
            }
        }
        if best.is_some() {
            return best;
        }
        if scopes[i].barrier {
            return None;
        }
        idx = scopes[i].parent;
    }
    None
}

/// How many scopes in the same (barrier-limited) chain [`lookup`] walks
/// hold a visible definition of `name` — see [`Binding::visible_defs`].
/// Unlike `lookup` this does NOT stop at the first hit: the whole point is
/// to notice a SECOND, shadowed binding further out.
fn count_visible_scopes(scopes: &[Scope], scope_idx: usize, name: &str, ref_start: usize) -> usize {
    let mut n = 0usize;
    let mut idx = Some(scope_idx);
    while let Some(i) = idx {
        if scopes[i]
            .defs
            .iter()
            .any(|d| d.name == name && def_visible(d, ref_start))
        {
            n += 1;
        }
        if scopes[i].barrier {
            break;
        }
        idx = scopes[i].parent;
    }
    n
}

fn def_visible(d: &Def, ref_start: usize) -> bool {
    // Never bind a reference to itself (same/overlapping identifier span).
    if ref_start >= d.start && ref_start < d.end {
        return false;
    }
    match d.visibility {
        // Hoisted: visible throughout the scope, including before the
        // definition in source order (function decls, params, Python).
        DefVisibility::EntireScope => true,
        // Sequential: only after the enclosing declaration ends, and the
        // def must appear before the ref in source order.
        DefVisibility::AfterDef => d.start < ref_start && d.visible_from <= ref_start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bind(lang: &str, src: &str) -> Vec<Binding> {
        bind_locals(lang, src.as_bytes()).expect("bind_locals")
    }

    fn binding_at<'a>(bs: &'a [Binding], ref_line: u32, name: &str) -> Option<&'a Binding> {
        bs.iter().find(|b| b.ref_line == ref_line && b.name == name)
    }

    fn def_line(bs: &[Binding], ref_line: u32, name: &str) -> Option<u32> {
        binding_at(bs, ref_line, name).map(|b| b.def_line)
    }

    // --- Rust ------------------------------------------------------------

    #[test]
    fn rust_param_shadows_module_fn() {
        let src = "\
fn value() -> i32 { 1 }
fn run(value: i32) -> i32 {
    value
}
";
        let bs = bind("rust", src);
        // The bare `value` on the return line binds to the parameter, not
        // the module-level function.
        assert_eq!(def_line(&bs, 3, "value"), Some(2));
    }

    #[test]
    fn rust_block_let_shadows_outer() {
        let src = "\
fn main() {
    let x = 1;
    {
        let x = 2;
        let _y = x;
    }
}
";
        let bs = bind("rust", src);
        // `_y = x` should bind to the inner `let x = 2`.
        assert_eq!(def_line(&bs, 5, "x"), Some(4));
    }

    #[test]
    fn rust_loop_var_shadows_param() {
        let src = "\
fn walk(item: i32) {
    for item in 0..3 {
        let _z = item;
    }
}
";
        let bs = bind("rust", src);
        assert_eq!(def_line(&bs, 3, "item"), Some(2));
    }

    #[test]
    fn rust_closure_captures_outer() {
        let src = "\
fn main() {
    let outer = 10;
    let f = || outer;
    let _ = f;
}
";
        let bs = bind("rust", src);
        // `outer` inside the closure body binds to the outer let.
        assert_eq!(def_line(&bs, 3, "outer"), Some(2));
    }

    #[test]
    fn rust_unbound_falls_through() {
        let src = "\
fn main() {
    let _x = missing;
}
";
        let bs = bind("rust", src);
        assert!(binding_at(&bs, 2, "missing").is_none());
    }

    #[test]
    fn rust_let_rhs_does_not_self_bind() {
        let src = "\
fn main() {
    let a = 1;
    let a = a;
}
";
        let bs = bind("rust", src);
        // RHS of the second `let a = a` must bind to the FIRST `a`, not self.
        assert_eq!(def_line(&bs, 3, "a"), Some(2));
    }

    #[test]
    fn rust_function_is_visible_before_its_declaration() {
        let src = "\
fn main() {
    let _x = later();
}
fn later() -> i32 { 1 }
";
        let bs = bind("rust", src);
        assert_eq!(def_line(&bs, 2, "later"), Some(4));
    }

    // --- TypeScript ------------------------------------------------------
    // The three shapes below mirror tests/fixtures/oracle/shadow.ts so the
    // oracle bar's same-file exact cases stay pinned under unit tests too.

    #[test]
    fn ts_arrow_param_shadows_outer() {
        let src = "\
const value = 1;
const f = (value: number) => value + 1;
";
        let bs = bind("typescript", src);
        // The `value` in the arrow body binds to the param.
        assert_eq!(def_line(&bs, 2, "value"), Some(2));
    }

    #[test]
    fn ts_function_param_shadows_module_const() {
        // oracle/shadow.ts: `function run(value)` shadows module `const value`.
        let src = "\
const value = 1;
function run(value: number): number {
  return value;
}
";
        let bs = bind("typescript", src);
        assert_eq!(def_line(&bs, 3, "value"), Some(2));
        // And not the module const on line 1.
        let b = binding_at(&bs, 3, "value").expect("bound");
        assert_eq!(b.def_line, 2);
        assert_ne!(b.def_line, 1);
    }

    #[test]
    fn ts_block_let_shadows_outer() {
        let src = "\
function main() {
  let x = 1;
  {
    let x = 2;
    const y = x;
  }
}
";
        let bs = bind("typescript", src);
        assert_eq!(def_line(&bs, 5, "x"), Some(4));
    }

    #[test]
    fn ts_block_let_shadows_outer_oracle_shape() {
        // oracle/shadow.ts blockShadow — identical nesting, void return.
        let src = "\
function blockShadow(): void {
  let x = 10;
  {
    let x = 20;
    const y = x;
  }
}
";
        let bs = bind("typescript", src);
        assert_eq!(def_line(&bs, 5, "x"), Some(4));
    }

    #[test]
    fn ts_function_decl_is_hoisted_in_scope() {
        let src = "\
function main() {
  const y = later();
  function later() { return 1; }
}
";
        let bs = bind("typescript", src);
        assert_eq!(def_line(&bs, 2, "later"), Some(3));
    }

    #[test]
    fn ts_function_decl_hoisted_before_nested_decl_oracle_shape() {
        // oracle/shadow.ts hoistCall — call site before nested function decl.
        let src = "\
function hoistCall(): number {
  return later();
  function later(): number {
    return 1;
  }
}
";
        let bs = bind("typescript", src);
        assert_eq!(def_line(&bs, 2, "later"), Some(3));
    }

    #[test]
    fn ts_let_is_not_visible_before_declaration() {
        let src = "\
function main() {
  const y = later;
  let later = 1;
}
";
        let bs = bind("typescript", src);
        // `later` on the const line must NOT bind to the subsequent let
        // (we do not model TDZ as a hard error — we simply leave it unbound).
        assert!(binding_at(&bs, 2, "later").is_none());
    }

    #[test]
    fn ts_unbound_falls_through() {
        let src = "\
function main() {
  const x = missing;
}
";
        let bs = bind("typescript", src);
        assert!(binding_at(&bs, 2, "missing").is_none());
    }

    // --- TSX (same grammar shapes as TS for these patterns) --------------

    #[test]
    fn tsx_arrow_param_binds() {
        let src = "\
export const Box = (label: string) => label;
";
        let bs = bind("tsx", src);
        assert_eq!(def_line(&bs, 1, "label"), Some(1));
    }

    // --- Python ----------------------------------------------------------

    #[test]
    fn python_param_shadows_module_fn() {
        let src = "\
def value():
    return 1

def run(value):
    return value
";
        let bs = bind("python", src);
        assert_eq!(def_line(&bs, 5, "value"), Some(4));
    }

    #[test]
    fn python_comprehension_target_is_isolated() {
        let src = "\
x = 1
ys = [x for x in range(3)]
z = x
";
        let bs = bind("python", src);
        // `x` in the comprehension body binds to the comprehension target.
        let comp_bind = bs
            .iter()
            .find(|b| b.name == "x" && b.ref_line == 2 && b.def_line == 2);
        assert!(
            comp_bind.is_some(),
            "comprehension body x should bind to comp target; got {bs:?}"
        );
        // `z = x` binds to the module-level `x = 1`, NOT the comp target.
        assert_eq!(def_line(&bs, 3, "x"), Some(1));
    }

    #[test]
    fn python_assignment_visible_before_in_scope() {
        // Python has no TDZ for plain names — a name assigned later in the
        // same function is still "bound" for our EntireScope model. We
        // model that for function-body assignments.
        let src = "\
def f():
    y = x
    x = 1
    return y
";
        let bs = bind("python", src);
        // EntireScope: `x` on line 2 binds to the assignment on line 3.
        assert_eq!(def_line(&bs, 2, "x"), Some(3));
    }

    #[test]
    fn python_unbound_falls_through() {
        let src = "\
def f():
    return missing
";
        let bs = bind("python", src);
        assert!(binding_at(&bs, 2, "missing").is_none());
    }

    #[test]
    fn python_nested_function_param_shadows() {
        let src = "\
def outer(x):
    def inner(x):
        return x
    return inner
";
        let bs = bind("python", src);
        assert_eq!(def_line(&bs, 3, "x"), Some(2));
    }

    #[test]
    fn python_attribute_field_does_not_bind_to_local() {
        // `self.name = name`: the attribute `name` is NOT a lexical ref to
        // the param; only the RHS `name` is.
        let src = "\
def __init__(self, name):
    self.name = name
";
        let bs = bind("python", src);
        let name_binds: Vec<_> = bs.iter().filter(|b| b.name == "name").collect();
        assert_eq!(name_binds.len(), 1, "only RHS should bind; got {bs:?}");
        // RHS sits after `= ` — column past the attribute.
        assert!(
            name_binds[0].ref_col > 10,
            "expected RHS name, got col {}",
            name_binds[0].ref_col
        );
        assert_eq!(name_binds[0].def_line, 1);
    }

    // --- Query compiles for every locals language ------------------------

    #[test]
    fn locals_query_compiles_for_every_proof_language() {
        for id in LOCALS_LANG_IDS {
            let (tree, language) = lang::parse(id, b"").unwrap();
            let q = lang::locals_query(id).expect(id);
            lang::compile_query(id, &language, q).unwrap();
            assert!(!tree.root_node().kind().is_empty());
            // Smoke: empty source produces no bindings, no panic.
            assert!(bind_locals(id, b"").unwrap().is_empty());
        }
    }

    #[test]
    fn supports_is_exactly_the_four_proof_languages() {
        for id in ["rust", "typescript", "tsx", "python"] {
            assert!(supports(id), "{id}");
        }
        for id in ["javascript", "ruby", "go", "bash", "yaml"] {
            assert!(!supports(id), "{id}");
        }
    }
}
