//! V3.G2 — arity + receiver scoring. Pure heuristics used as a RANK scorer
//! (never a hard gate): candidates whose param range cannot accept the
//! call-site arg count are DEMOTED to `class=candidate` (never dropped);
//! method-call receivers bound by G1 locals to a def with a syntactically
//! evident type name BOOST matching `container` candidates.

/// Parse a cut signature string (from `extract::build_signature`) into
/// `(param_min, param_max)`.
///
/// Rules (conservative — classify DOWN when unsure):
/// - No `(` → `(None, None)` (not a callable-looking signature).
/// - Empty param list `()` → `(Some(0), Some(0))`.
/// - Count comma-separated top-level params inside the first `(...)`.
/// - A param with `=` (default) contributes to max but not min once
///   defaults start (Rust/TS/Python defaults); params before the first
///   default are required.
/// - `...rest` / `*args` / `**kwargs` / `&rest` → max becomes unbounded
///   (`None`); min is the required count before the rest.
/// - `self` / `cls` / `&self` / `&mut self` / `mut self` (Rust receiver)
///   are NOT counted as call-site args.
/// - Angle-bracket / paren nesting is respected so `fn f(x: HashMap<A, B>)`
///   counts as 1 param.
pub fn param_range_from_signature(sig: &str) -> (Option<u32>, Option<u32>) {
    let Some(open) = sig.find('(') else {
        return (None, None);
    };
    let rest = &sig[open + 1..];
    let Some(close_rel) = find_matching_paren(rest) else {
        return (None, None);
    };
    let inside = rest[..close_rel].trim();
    if inside.is_empty() {
        return (Some(0), Some(0));
    }
    let parts = split_top_level_commas(inside);
    let mut required = 0u32;
    let mut optional = 0u32;
    let mut unbounded = false;
    let mut seen_default = false;
    for part in parts {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        // Receivers — not call-site args.
        if is_receiver_param(p) {
            continue;
        }
        // Rest / varargs → open max.
        if is_varargs_param(p) {
            unbounded = true;
            continue;
        }
        let has_default = p.contains('=');
        if has_default {
            seen_default = true;
            optional += 1;
        } else if seen_default {
            // A required after a default is unusual; count as optional so
            // we never under-estimate the max.
            optional += 1;
        } else {
            required += 1;
        }
    }
    let min = required;
    let max = if unbounded {
        None
    } else {
        Some(required + optional)
    };
    (Some(min), max)
}

/// `true` when `arg_count` cannot fit in `[min, max]` (when both known).
/// Unknown ranges never demote.
pub fn arity_rejects(arg_count: u32, param_min: Option<u32>, param_max: Option<u32>) -> bool {
    if let Some(min) = param_min {
        if arg_count < min {
            return true;
        }
    }
    if let Some(max) = param_max {
        if arg_count > max {
            return true;
        }
    }
    false
}

/// Count call-expression arguments whose callee identifier covers
/// `(line, col)` (1-based line, 0-based col). Returns `None` when the
/// position is not a call-site callee (or the language parse fails).
pub fn call_arg_count_at(lang_id: &str, source: &[u8], line: u32, col: u32) -> Option<u32> {
    let (tree, _) = crate::lang::parse(lang_id, source).ok()?;
    let root = tree.root_node();
    let point = tree_sitter::Point {
        row: (line.saturating_sub(1)) as usize,
        column: col as usize,
    };
    let node = root.named_descendant_for_point_range(point, point)?;
    // Walk up from the identifier to a call expression.
    let mut cur = node;
    let mut ident_node = None;
    // Prefer the identifier itself if we're on one.
    if matches!(
        cur.kind(),
        "identifier" | "property_identifier" | "field_identifier" | "type_identifier"
    ) {
        ident_node = Some(cur);
    }
    loop {
        let kind = cur.kind();
        if is_call_kind(lang_id, kind) {
            // Ensure the callee covers our token (function name or method
            // property), not an argument.
            if let Some(id) = ident_node {
                if !callee_covers(lang_id, cur, id) {
                    // Maybe an outer call — keep climbing.
                    if let Some(p) = cur.parent() {
                        cur = p;
                        continue;
                    }
                    return None;
                }
            }
            return Some(count_args(lang_id, cur, source));
        }
        if let Some(p) = cur.parent() {
            if matches!(
                p.kind(),
                "identifier" | "property_identifier" | "field_identifier" | "type_identifier"
            ) {
                ident_node = Some(p);
            }
            cur = p;
        } else {
            return None;
        }
    }
}

/// When the receiver of a method call is a locally-bound identifier whose
/// definition has a syntactically evident type name (`let x: Foo`,
/// `x = Foo(...)`, `new Foo(...)`), return that type name. Used to BOOST
/// candidates whose `container` matches. `local_def_line` is the 1-based
/// line of the local def (from G1 `local_def_ordinal` → occurrence lookup).
pub fn receiver_type_hint(
    lang_id: &str,
    source: &[u8],
    _line: u32,
    _col: u32,
    local_def_line: Option<u32>,
) -> Option<String> {
    let def_line = local_def_line?;
    let text = std::str::from_utf8(source).ok()?;
    let line_text = text.lines().nth((def_line.saturating_sub(1)) as usize)?;
    type_name_from_def_line(lang_id, line_text)
}

/// Pure line-text type extraction (unit-testable without a tree).
pub fn type_name_from_def_line(lang_id: &str, line: &str) -> Option<String> {
    let line = line.trim();
    // `let x: Foo` / `let x: Foo<T>` / `const x: Foo =` / `var x: Foo`
    if let Some(after_colon) = line.split_once(':').map(|(_, r)| r.trim()) {
        // Avoid matching `::` path seps — require a type-ish start.
        if !after_colon.starts_with(':') {
            if let Some(name) = leading_type_ident(after_colon) {
                return Some(name);
            }
        }
    }
    // `x = Foo(...)` / `x = new Foo(...)` / `x = Foo::new(...)`
    if let Some(after_eq) = line.split_once('=').map(|(_, r)| r.trim()) {
        let after_eq = after_eq
            .strip_prefix("new ")
            .map(str::trim)
            .unwrap_or(after_eq);
        if let Some(name) = leading_type_ident(after_eq) {
            // Rust `Foo::new` — take Foo.
            if name
                .chars()
                .next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false)
                || matches!(lang_id, "typescript" | "tsx" | "javascript" | "python")
            {
                return Some(name);
            }
        }
    }
    // Python `x = Foo(` already covered; `x: Foo =` covered by colon arm.
    let _ = lang_id;
    None
}

// --- internals ---------------------------------------------------------------

fn is_receiver_param(p: &str) -> bool {
    let p = p.trim();
    matches!(
        p,
        "self" | "cls" | "Self" | "&self" | "&mut self" | "mut self" | "&'a self" | "&'static self"
    ) || p.starts_with("&self")
        || p.starts_with("&mut self")
        || p.starts_with("&'") && p.contains("self")
}

fn is_varargs_param(p: &str) -> bool {
    let p = p.trim();
    p.starts_with("...")
        || p.starts_with("*args")
        || p.starts_with("**kwargs")
        || p.starts_with("*")
        || p.starts_with("&rest")
        || p.contains("...")
}

fn find_matching_paren(s: &str) -> Option<usize> {
    let mut depth = 0i32;
    let mut angle = 0i32;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => angle += 1,
            '>' => angle = (angle - 1).max(0),
            '(' if angle == 0 => depth += 1,
            ')' if angle == 0 => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut paren = 0i32;
    let mut angle = 0i32;
    let mut bracket = 0i32;
    let mut brace = 0i32;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => paren += 1,
            ')' => paren -= 1,
            '<' => angle += 1,
            '>' => angle -= 1,
            '[' => bracket += 1,
            ']' => bracket -= 1,
            '{' => brace += 1,
            '}' => brace -= 1,
            ',' if paren == 0 && angle == 0 && bracket == 0 && brace == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn leading_type_ident(s: &str) -> Option<String> {
    let s = s.trim();
    let mut chars = s.chars().peekable();
    let first = *chars.peek()?;
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return None;
    }
    let mut name = String::new();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
            name.push(c);
            chars.next();
        } else {
            break;
        }
    }
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Whether `kind` is a call/new expression node for `lang_id`. Public so
/// hierarchy extraction can walk the same call set without duplicating the
/// language table.
pub fn is_call_kind(lang_id: &str, kind: &str) -> bool {
    match lang_id {
        "rust" => matches!(kind, "call_expression" | "macro_invocation"),
        "python" => matches!(kind, "call"),
        "typescript" | "tsx" | "javascript" => {
            matches!(kind, "call_expression" | "new_expression")
        }
        "go" => matches!(kind, "call_expression"),
        _ => matches!(kind, "call_expression" | "call" | "new_expression"),
    }
}

fn callee_covers(lang_id: &str, call: tree_sitter::Node<'_>, ident: tree_sitter::Node<'_>) -> bool {
    // function field (rust/ts/go) or first child.
    if let Some(func) = call.child_by_field_name("function") {
        return node_contains(func, ident);
    }
    if lang_id == "python" {
        // python `call` has `function` field in tree-sitter-python.
        if let Some(func) = call.child_by_field_name("function") {
            return node_contains(func, ident);
        }
    }
    // Fallback: any non-arguments child that covers ident.
    let mut cursor = call.walk();
    for child in call.named_children(&mut cursor) {
        if matches!(child.kind(), "arguments" | "argument_list") {
            continue;
        }
        if node_contains(child, ident) {
            return true;
        }
    }
    false
}

fn node_contains(outer: tree_sitter::Node<'_>, inner: tree_sitter::Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && outer.end_byte() >= inner.end_byte()
}

/// Count positional arguments of a call/new expression node. Shared with
/// hierarchy extraction — do NOT reimplement the paren-list walk elsewhere.
pub fn count_args(lang_id: &str, call: tree_sitter::Node<'_>, source: &[u8]) -> u32 {
    let args_node = call
        .child_by_field_name("arguments")
        .or_else(|| call.child_by_field_name("argument_list"));
    let args_node = match args_node {
        Some(n) => Some(n),
        None => {
            let mut c = call.walk();
            let mut found = None;
            for n in call.named_children(&mut c) {
                if matches!(n.kind(), "arguments" | "argument_list") {
                    found = Some(n);
                    break;
                }
            }
            found
        }
    };
    let Some(args) = args_node else {
        let _ = (lang_id, source);
        return 0;
    };
    let mut count = 0u32;
    let mut c = args.walk();
    for child in args.named_children(&mut c) {
        // Skip keyword-only punctuation nodes; count real arg expressions.
        if matches!(child.kind(), "," | "(" | ")" | "[" | "]" | "comment") {
            continue;
        }
        count += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_params() {
        assert_eq!(param_range_from_signature("fn foo()"), (Some(0), Some(0)));
        assert_eq!(
            param_range_from_signature("function foo()"),
            (Some(0), Some(0))
        );
    }

    #[test]
    fn required_params() {
        assert_eq!(
            param_range_from_signature("fn add(a: i32, b: i32)"),
            (Some(2), Some(2))
        );
        assert_eq!(
            param_range_from_signature("def add(a, b):"),
            (Some(2), Some(2))
        );
    }

    #[test]
    fn rust_self_not_counted() {
        assert_eq!(
            param_range_from_signature("fn area(&self, scale: f64)"),
            (Some(1), Some(1))
        );
        assert_eq!(
            param_range_from_signature("fn new(self)"),
            (Some(0), Some(0))
        );
    }

    #[test]
    fn defaults_open_min() {
        assert_eq!(
            param_range_from_signature("fn f(a: i32, b: i32 = 1)"),
            (Some(1), Some(2))
        );
        assert_eq!(
            param_range_from_signature("def f(a, b=1, c=2):"),
            (Some(1), Some(3))
        );
    }

    #[test]
    fn varargs_unbounded_max() {
        let (min, max) = param_range_from_signature("fn f(a: i32, ...rest: i32[])");
        assert_eq!(min, Some(1));
        assert_eq!(max, None);
        let (min, max) = param_range_from_signature("def f(a, *args):");
        assert_eq!(min, Some(1));
        assert_eq!(max, None);
    }

    #[test]
    fn nested_generics_one_param() {
        assert_eq!(
            param_range_from_signature("fn f(x: HashMap<A, B>)"),
            (Some(1), Some(1))
        );
    }

    #[test]
    fn arity_rejects_outside_range() {
        assert!(arity_rejects(0, Some(1), Some(2)));
        assert!(arity_rejects(3, Some(1), Some(2)));
        assert!(!arity_rejects(1, Some(1), Some(2)));
        assert!(!arity_rejects(5, Some(1), None)); // unbounded max
        assert!(!arity_rejects(0, None, None)); // unknown never rejects
    }

    #[test]
    fn type_name_from_let_annotation() {
        assert_eq!(
            type_name_from_def_line("rust", "let x: Foo = Foo::new();"),
            Some("Foo".into())
        );
        assert_eq!(
            type_name_from_def_line("typescript", "const x: Bar = new Bar();"),
            Some("Bar".into())
        );
    }

    #[test]
    fn type_name_from_constructor_assign() {
        assert_eq!(
            type_name_from_def_line("typescript", "const x = new Widget();"),
            Some("Widget".into())
        );
        assert_eq!(
            type_name_from_def_line("python", "x = Foo(1, 2)"),
            Some("Foo".into())
        );
    }

    #[test]
    fn call_arg_count_rust() {
        let src = b"fn main() { foo(1, 2, 3); }\nfn foo(a: i32, b: i32, c: i32) {}\n";
        // "foo" at the call site — line 1, col of foo
        // `fn main() { foo(1, 2, 3); }` — foo starts at byte offset after "{ "
        let line = 1u32;
        let col = {
            let s = std::str::from_utf8(src).unwrap();
            s.find("foo(").map(|j| j as u32).unwrap()
        };
        assert_eq!(call_arg_count_at("rust", src, line, col), Some(3));
    }
}
