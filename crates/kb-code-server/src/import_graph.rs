//! V3.G2 — import-graph extraction (content-addressed specs) + resolution
//! (repo-addressed edges).
//!
//! Extraction walks the same import forms `crate::imports` already
//! understands for the on-demand origin ladder, but records EVERY import
//! statement (not just the one matching a clicked identifier). Resolution
//! reuses [`crate::imports::resolve_module_file`] so the path ladder stays
//! single-homed.
//!
//! # Out of scope (v3.0)
//! - TypeScript `tsconfig` path aliases (`paths` / `baseUrl`)
//! - Macro-generated / cfg-gated Rust modules (leave unresolved)
//! - node_modules / Cargo.toml / pip package resolution

use crate::imports;
use crate::lang;
use std::path::Path;
use tree_sitter::Node;

/// One content-addressed import-spec row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSpec {
    pub ordinal: u32,
    /// Module path ready for [`imports::resolve_module_file`].
    pub raw_spec: String,
    /// `"use"` | `"mod"` | `"import"` | `"from"` | `"require"` | `"export_from"`.
    pub kind: String,
}

/// Extract every import statement from `source` for a supported language.
/// Empty for unsupported languages (no error).
pub fn extract_import_specs(lang_id: &str, source: &[u8]) -> Vec<ImportSpec> {
    if !imports::supports(lang_id) {
        return Vec::new();
    }
    let mut specs = match lang_id {
        "rust" => rust_specs(source),
        "typescript" | "tsx" | "javascript" => ts_specs(lang_id, source),
        "python" => python_specs(source),
        "go" => go_specs(source),
        _ => Vec::new(),
    };
    // Deterministic: stable by appearance order (walker order), re-number.
    for (i, s) in specs.iter_mut().enumerate() {
        s.ordinal = i as u32;
    }
    // Dedup identical (raw_spec, kind) keeping first ordinal.
    let mut seen = std::collections::HashSet::new();
    specs.retain(|s| seen.insert((s.raw_spec.clone(), s.kind.clone())));
    for (i, s) in specs.iter_mut().enumerate() {
        s.ordinal = i as u32;
    }
    specs
}

/// Resolve specs against the live filesystem + files table paths. Returns
/// `(raw_spec, target_repo_rel_path)` pairs for specs that resolve to an
/// existing file inside the repo. Unresolvable specs are skipped (the
/// content-addressed spec row still exists as evidence).
pub fn resolve_import_edges(
    repo_root: &Path,
    current_file: &Path,
    lang_id: &str,
    specs: &[ImportSpec],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen_targets = std::collections::HashSet::new();
    for spec in specs {
        if let Some(rel) =
            imports::resolve_module_file(repo_root, current_file, lang_id, &spec.raw_spec)
        {
            if let Some(rel_str) = rel.to_str() {
                // One edge per (raw_spec, target); also collapse duplicate
                // targets from glob-ish forms to a single row per target.
                let key = (spec.raw_spec.clone(), rel_str.to_string());
                if seen_targets.insert(key.clone()) {
                    out.push(key);
                }
            }
        }
    }
    out
}

// --- Rust -------------------------------------------------------------------

fn rust_specs(source: &[u8]) -> Vec<ImportSpec> {
    let Ok((tree, _)) = lang::parse(lang::RUST.id, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_rust(tree.root_node(), source, &mut out);
    out
}

fn walk_rust(node: Node<'_>, source: &[u8], out: &mut Vec<ImportSpec>) {
    match node.kind() {
        "use_declaration" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                collect_rust_use(argument, "", source, out);
            }
        }
        "mod_item" if node.child_by_field_name("body").is_none() => {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    out.push(ImportSpec {
                        ordinal: 0,
                        raw_spec: format!("self::{name}"),
                        kind: "mod".into(),
                    });
                }
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_rust(child, source, out);
    }
}

fn collect_rust_use(node: Node<'_>, prefix: &str, source: &[u8], out: &mut Vec<ImportSpec>) {
    match node.kind() {
        "identifier" => {
            if let Ok(name) = node.utf8_text(source) {
                let raw = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}::{name}")
                };
                out.push(ImportSpec {
                    ordinal: 0,
                    raw_spec: raw,
                    kind: "use".into(),
                });
            }
        }
        "scoped_identifier" | "scoped_type_identifier" => {
            if let Ok(text) = node.utf8_text(source) {
                let raw = if prefix.is_empty() {
                    text.to_string()
                } else {
                    format!("{prefix}::{text}")
                };
                out.push(ImportSpec {
                    ordinal: 0,
                    raw_spec: raw,
                    kind: "use".into(),
                });
            }
        }
        "use_as_clause" => {
            // `D as E` — record the original path (D side).
            if let Some(path) = node.child_by_field_name("path") {
                collect_rust_use(path, prefix, source, out);
            } else if let Some(first) = node.named_child(0) {
                collect_rust_use(first, prefix, source, out);
            }
        }
        "use_wildcard" => {
            // `use a::b::*` — edge to the module path without the star.
            if !prefix.is_empty() {
                out.push(ImportSpec {
                    ordinal: 0,
                    raw_spec: prefix.to_string(),
                    kind: "use".into(),
                });
            }
        }
        "use_list" => {
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                collect_rust_use(child, prefix, source, out);
            }
        }
        "scoped_use_list" => {
            let path_text = node
                .child_by_field_name("path")
                .and_then(|p| p.utf8_text(source).ok())
                .unwrap_or("");
            let new_prefix = if prefix.is_empty() {
                path_text.to_string()
            } else if path_text.is_empty() {
                prefix.to_string()
            } else {
                format!("{prefix}::{path_text}")
            };
            if let Some(list) = node.child_by_field_name("list") {
                collect_rust_use(list, &new_prefix, source, out);
            } else {
                let mut c = node.walk();
                for child in node.named_children(&mut c) {
                    if child.kind() == "use_list" {
                        collect_rust_use(child, &new_prefix, source, out);
                    }
                }
            }
        }
        _ => {
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                collect_rust_use(child, prefix, source, out);
            }
        }
    }
}

// --- TypeScript / TSX / JS --------------------------------------------------

fn ts_specs(lang_id: &str, source: &[u8]) -> Vec<ImportSpec> {
    let Ok((tree, _)) = lang::parse(lang_id, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_ts(tree.root_node(), source, &mut out);
    out
}

fn walk_ts(node: Node<'_>, source: &[u8], out: &mut Vec<ImportSpec>) {
    match node.kind() {
        "import_statement" => {
            if let Some(spec) = ts_source_spec(node, source) {
                out.push(ImportSpec {
                    ordinal: 0,
                    raw_spec: spec,
                    kind: "import".into(),
                });
            }
        }
        "export_statement" => {
            if let Some(spec) = ts_source_spec(node, source) {
                out.push(ImportSpec {
                    ordinal: 0,
                    raw_spec: spec,
                    kind: "export_from".into(),
                });
            }
        }
        "call_expression" => {
            // require("./x")
            if let Some(func) = node.child_by_field_name("function") {
                if func.kind() == "identifier" {
                    if let Ok("require") = func.utf8_text(source) {
                        if let Some(args) = node.child_by_field_name("arguments") {
                            if let Some(arg) = args.named_child(0) {
                                if let Some(spec) = string_literal_relative(arg, source) {
                                    out.push(ImportSpec {
                                        ordinal: 0,
                                        raw_spec: spec,
                                        kind: "require".into(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_ts(child, source, out);
    }
}

fn ts_source_spec(node: Node<'_>, source: &[u8]) -> Option<String> {
    let src_node = node.child_by_field_name("source")?;
    string_literal_relative(src_node, source)
}

fn string_literal_relative(node: Node<'_>, source: &[u8]) -> Option<String> {
    let raw = node.utf8_text(source).ok()?;
    let trimmed = raw.trim_matches(|c| c == '"' || c == '\'' || c == '`');
    if trimmed.starts_with("./") || trimmed.starts_with("../") {
        Some(trimmed.to_string())
    } else {
        // Still record absolute-ish package specs as evidence, but they
        // won't resolve (resolve_module_file only follows relatives for TS).
        // For the graph we only want resolvable edges; skip non-relative.
        None
    }
}

// --- Python -----------------------------------------------------------------

fn python_specs(source: &[u8]) -> Vec<ImportSpec> {
    let Ok((tree, _)) = lang::parse(lang::PYTHON.id, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_py(tree.root_node(), source, &mut out);
    out
}

fn walk_py(node: Node<'_>, source: &[u8], out: &mut Vec<ImportSpec>) {
    match node.kind() {
        "import_statement" => {
            let mut c = node.walk();
            for child in node.children_by_field_name("name", &mut c) {
                if let Some(path) = py_import_path(child, source) {
                    out.push(ImportSpec {
                        ordinal: 0,
                        raw_spec: path,
                        kind: "import".into(),
                    });
                }
            }
        }
        "import_from_statement" => {
            if let Some(module_node) = node.child_by_field_name("module_name") {
                if let Some(module_path) = py_module_path(module_node, source) {
                    // Record the module itself (for edges to the package file).
                    out.push(ImportSpec {
                        ordinal: 0,
                        raw_spec: module_path.clone(),
                        kind: "from".into(),
                    });
                    // Also record `module/name` for each imported name so
                    // resolve's greedy ladder can hit submodules.
                    let mut c = node.walk();
                    for child in node.children_by_field_name("name", &mut c) {
                        if let Some(name) = py_from_name(child, source) {
                            out.push(ImportSpec {
                                ordinal: 0,
                                raw_spec: format!("{module_path}/{name}"),
                                kind: "from".into(),
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_py(child, source, out);
    }
}

fn py_import_path(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "dotted_name" => Some(dotted_slash(node, source)),
        "aliased_import" => node
            .child_by_field_name("name")
            .map(|n| dotted_slash(n, source)),
        _ => None,
    }
}

fn py_from_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "dotted_name" => node.utf8_text(source).ok().map(str::to_string),
        "aliased_import" => node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .map(str::to_string),
        _ => None,
    }
}

fn py_module_path(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "dotted_name" => Some(dotted_slash(node, source)),
        "relative_import" => {
            let mut dots = String::new();
            let mut rest = String::new();
            let mut c = node.walk();
            for child in node.named_children(&mut c) {
                match child.kind() {
                    "import_prefix" => dots = child.utf8_text(source).ok()?.to_string(),
                    "dotted_name" => rest = dotted_slash(child, source),
                    _ => {}
                }
            }
            if dots.is_empty() {
                return None;
            }
            Some(format!("{dots}/{rest}"))
        }
        _ => None,
    }
}

fn dotted_slash(node: Node<'_>, source: &[u8]) -> String {
    let mut c = node.walk();
    node.named_children(&mut c)
        .filter_map(|n| n.utf8_text(source).ok())
        .collect::<Vec<_>>()
        .join("/")
}

// --- Go ---------------------------------------------------------------------

fn go_specs(source: &[u8]) -> Vec<ImportSpec> {
    let Ok((tree, _)) = lang::parse(lang::GO.id, source) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_go(tree.root_node(), source, &mut out);
    out
}

fn walk_go(node: Node<'_>, source: &[u8], out: &mut Vec<ImportSpec>) {
    if node.kind() == "import_spec" {
        if let Some(path_node) = node.child_by_field_name("path") {
            if let Ok(raw) = path_node.utf8_text(source) {
                let trimmed = raw.trim_matches('"');
                if !trimmed.is_empty() {
                    out.push(ImportSpec {
                        ordinal: 0,
                        raw_spec: trimmed.to_string(),
                        kind: "import".into(),
                    });
                }
            }
        }
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk_go(child, source, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, content: &str) {
        let abs = root.join(rel);
        if let Some(p) = abs.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(abs, content).unwrap();
    }

    #[test]
    fn rust_use_and_mod_specs() {
        let src = b"use crate::util::helper;\nmod child;\nuse super::sib::{A, B as C};\n";
        let specs = extract_import_specs("rust", src);
        let raws: Vec<_> = specs.iter().map(|s| s.raw_spec.as_str()).collect();
        assert!(raws
            .iter()
            .any(|r| r.contains("util::helper") || *r == "crate::util::helper"));
        assert!(raws.contains(&"self::child"));
    }

    #[test]
    fn ts_relative_import_and_export_from() {
        let src = b"import { foo } from './util';\nexport { bar } from '../lib';\nimport x from 'lodash';\n";
        let specs = extract_import_specs("typescript", src);
        assert!(specs
            .iter()
            .any(|s| s.raw_spec == "./util" && s.kind == "import"));
        assert!(specs
            .iter()
            .any(|s| s.raw_spec == "../lib" && s.kind == "export_from"));
        // bare package not recorded
        assert!(!specs.iter().any(|s| s.raw_spec.contains("lodash")));
    }

    #[test]
    fn python_absolute_and_relative() {
        let src = b"import pkg.mod\nfrom .sub import helper\nfrom pkg import X\n";
        let specs = extract_import_specs("python", src);
        assert!(specs.iter().any(|s| s.raw_spec == "pkg/mod"));
        assert!(specs
            .iter()
            .any(|s| s.raw_spec.starts_with("./") || s.raw_spec.starts_with(".")));
    }

    #[test]
    fn resolve_ts_edge_to_index() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/app.ts", "import { x } from './lib';\n");
        write(root, "src/lib/index.ts", "export const x = 1;\n");
        let specs = extract_import_specs("typescript", b"import { x } from './lib';\n");
        let edges = resolve_import_edges(root, Path::new("src/app.ts"), "typescript", &specs);
        assert!(
            edges.iter().any(|(_, t)| t == "src/lib/index.ts"),
            "edges={edges:?}"
        );
    }

    #[test]
    fn resolve_rust_crate_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "src/lib.rs", "mod util;\nuse crate::util::helper;\n");
        write(root, "src/util.rs", "pub fn helper() {}\n");
        let specs = extract_import_specs("rust", b"mod util;\nuse crate::util::helper;\n");
        let edges = resolve_import_edges(root, Path::new("src/lib.rs"), "rust", &specs);
        assert!(
            edges.iter().any(|(_, t)| t == "src/util.rs"),
            "edges={edges:?} specs={specs:?}"
        );
    }
}
