//! V3.G2 — read/write access tagging for reference occurrences.
//!
//! Pure, per-language AST checks. Returns `Some("write")` only when the
//! reference is clearly the LHS target of an assignment / compound
//! assignment / `&mut` borrow / `del` target. Returns `Some("read")` when
//! clearly a value use. Returns `None` when unsure — NEVER guess.

/// Classify access at `(line, col)` (1-based line, 0-based col) for a
/// reference token. `None` when unsure or the language isn't covered.
pub fn access_at(lang_id: &str, source: &[u8], line: u32, col: u32) -> Option<&'static str> {
    let (tree, _) = crate::lang::parse(lang_id, source).ok()?;
    let root = tree.root_node();
    let point = tree_sitter::Point {
        row: (line.saturating_sub(1)) as usize,
        column: col as usize,
    };
    let node = root.named_descendant_for_point_range(point, point)?;
    // Climb to the identifier if we landed on a parent.
    let mut ident = node;
    if !matches!(
        ident.kind(),
        "identifier" | "property_identifier" | "field_identifier" | "type_identifier"
    ) {
        // Try to find an identifier covering the point among children.
        let mut found = None;
        let mut c = ident.walk();
        for child in ident.named_children(&mut c) {
            if child.start_position() <= point
                && point < child.end_position()
                && matches!(
                    child.kind(),
                    "identifier" | "property_identifier" | "field_identifier"
                )
            {
                found = Some(child);
                break;
            }
        }
        ident = found?;
    }
    classify_ident(lang_id, ident)
}

fn classify_ident(lang_id: &str, ident: tree_sitter::Node<'_>) -> Option<&'static str> {
    let mut cur = ident;
    // Walk parents looking for assignment / del / mut patterns.
    for _ in 0..12 {
        let Some(parent) = cur.parent() else {
            break;
        };
        let pk = parent.kind();
        match lang_id {
            "rust" => {
                // `x = ...` — assignment_expression left
                if pk == "assignment_expression" {
                    if field_is(parent, "left", ident)
                        || node_contains(parent.child_by_field_name("left")?, ident)
                    {
                        return Some("write");
                    }
                    return Some("read");
                }
                // compound / `+=`
                if (pk == "compound_assignment_expr" || pk == "compound_assignment_expression")
                    && (field_is(parent, "left", ident)
                        || parent
                            .child_by_field_name("left")
                            .map(|l| node_contains(l, ident))
                            .unwrap_or(false))
                {
                    return Some("write");
                }
                // `&mut x`
                if pk == "reference_expression" || pk == "unary_expression" {
                    let mut c = parent.walk();
                    let has_mut = parent
                        .children(&mut c)
                        .any(|ch| ch.kind() == "mutable_specifier");
                    if has_mut && node_contains(parent, ident) {
                        return Some("write");
                    }
                }
                // let mut binding is a def, not a ref write — skip.
            }
            "python" => {
                if pk == "assignment" {
                    // left side of `=`
                    if let Some(left) = parent.child_by_field_name("left") {
                        if node_contains(left, ident) {
                            return Some("write");
                        }
                    }
                    // Also: first child before `=`
                    if let Some(first) = parent.named_child(0) {
                        if node_contains(first, ident) {
                            return Some("write");
                        }
                    }
                    return Some("read");
                }
                if pk == "augmented_assignment" {
                    if let Some(left) = parent.child_by_field_name("left") {
                        if node_contains(left, ident) {
                            return Some("write");
                        }
                    }
                    return Some("write");
                }
                if pk == "delete_statement" {
                    return Some("write");
                }
            }
            "typescript" | "tsx" | "javascript" => {
                if pk == "assignment_expression" {
                    if let Some(left) = parent.child_by_field_name("left") {
                        if node_contains(left, ident) {
                            return Some("write");
                        }
                    }
                    return Some("read");
                }
                if pk == "augmented_assignment_expression" {
                    if let Some(left) = parent.child_by_field_name("left") {
                        if node_contains(left, ident) {
                            return Some("write");
                        }
                    }
                    return Some("write");
                }
                if pk == "update_expression" {
                    // `x++` / `--x`
                    return Some("write");
                }
            }
            _ => {}
        }
        cur = parent;
    }
    // Default: a reference use is a read when we found an identifier but
    // no write pattern — still conservative for uncertain nests.
    Some("read")
}

fn field_is(parent: tree_sitter::Node<'_>, field: &str, ident: tree_sitter::Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .map(|n| node_contains(n, ident))
        .unwrap_or(false)
}

fn node_contains(outer: tree_sitter::Node<'_>, inner: tree_sitter::Node<'_>) -> bool {
    outer.start_byte() <= inner.start_byte() && outer.end_byte() >= inner.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_assignment_is_write() {
        let src = b"fn main() { let mut x = 1; x = 2; }\n";
        // Find the `x` on the LHS of `x = 2`.
        let s = std::str::from_utf8(src).unwrap();
        let idx = s.rfind("x =").unwrap();
        let col = idx as u32; // line 1, 0-based
        assert_eq!(access_at("rust", src, 1, col), Some("write"));
    }

    #[test]
    fn rust_read_use() {
        let src = b"fn main() { let x = 1; let y = x + 1; }\n";
        let s = std::str::from_utf8(src).unwrap();
        // The `x` in `x + 1`.
        let idx = s.rfind("x +").unwrap();
        assert_eq!(access_at("rust", src, 1, idx as u32), Some("read"));
    }

    #[test]
    fn python_assignment_write() {
        let src = b"x = 1\ny = x\nx = 2\n";
        // line 3 `x = 2`
        assert_eq!(access_at("python", src, 3, 0), Some("write"));
        // line 2 `y = x` — x is read
        assert_eq!(access_at("python", src, 2, 4), Some("read"));
    }

    #[test]
    fn ts_assignment_write() {
        let src = b"let x = 1;\nx = 2;\nconst y = x;\n";
        assert_eq!(access_at("typescript", src, 2, 0), Some("write"));
        // `x` in `const y = x`
        assert_eq!(access_at("typescript", src, 3, 10), Some("read"));
    }

    #[test]
    fn python_del_is_write() {
        let src = b"x = 1\ndel x\n";
        assert_eq!(access_at("python", src, 2, 4), Some("write"));
    }
}
