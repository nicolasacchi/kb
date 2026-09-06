//! PRR-N4 — `spec_subject`: `spec/**/*_spec.rb` → its subject source file
//! (`EdgeKind::SpecSubject`).
//!
//! Two resolution strategies, tried in order:
//!
//! 1. **`described_class`** (`RSpec.describe Foo do … end` / bare
//!    `describe Foo do … end`, first argument a CONSTANT, not a string
//!    description) — the strongest signal: an explicit assertion of
//!    subject identity. The resolved constant is searched across every
//!    standard Rails `app/*` role directory (models/controllers/jobs/
//!    mailers/helpers/components) PLUS `lib` — Rails class names always
//!    embed their own role suffix as part of the constant
//!    (`UsersController`, `FooJob`, `FooHelper`), so underscoring the
//!    WHOLE constant reconstructs the filename uniformly with no
//!    per-role-suffix special-casing needed.
//! 2. **Path convention** (`spec/models/x_spec.rb` → `app/models/x.rb`) —
//!    only for the handful of spec top-level directories that map 1:1 onto
//!    an `app/*` role (`models`/`controllers`/`helpers`/`jobs`/`mailers`/
//!    `components`/`services`) or `lib`; `requests`/`features`/`system`/
//!    `factories`/`support`/`views` spec dirs don't name a single subject
//!    class by path alone and are an honest, documented drop when
//!    `described_class` resolution also didn't find anything.
//!
//! When `described_class` resolves, it wins outright (path convention is
//! not ALSO tried) — an explicit `describe Foo` is a stronger claim than a
//! directory-name guess. Every candidate is existence-verified against
//! disk before being emitted (never fabricated); ambiguity (more than one
//! `app/*` role directory holding a same-named file — rare, but possible
//! for an unusually generic class name) is `Trust::Candidate`.

use crate::frameworks::rails::support::{
    call_args, call_method_name, constant_text, src_line, underscore_segment,
};
use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::path::Path;
use tree_sitter::Node;

/// Search order matters only for the `Trust::Candidate`'s first-listed
/// `dst_path` (deterministic, not a priority ranking) — see the module
/// doc's ambiguity note.
const APP_ROLE_DIRS: &[&str] = &[
    "app/models",
    "app/controllers",
    "app/jobs",
    "app/mailers",
    "app/helpers",
    "app/components",
    "app/services",
    "lib",
];

/// `path` must already be gated to `spec/**/*_spec.rb` (`rails::is_spec_file`).
pub fn extract(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    if let Ok((tree, _language)) = crate::lang::parse("ruby", bytes) {
        if let Some(edge) = resolve_described_class(&tree, bytes, path, repo_root) {
            return vec![edge];
        }
    }
    resolve_path_convention(path, repo_root)
        .into_iter()
        .collect()
}

fn resolve_described_class(
    tree: &tree_sitter::Tree,
    source: &[u8],
    path: &str,
    repo_root: &Path,
) -> Option<FrameworkEdge> {
    let mut found: Option<(String, u32)> = None;
    find_describe_call(tree.root_node(), source, &mut found);
    let (const_name, line) = found?;
    let candidates = search_app_role_dirs(repo_root, &const_name);
    make_subject_edge(path, Some(line), candidates, "described_class")
}

/// Depth-first, pre-order search for the FIRST `describe`/`RSpec.describe`
/// call whose first argument is a constant (not a string description) —
/// document order naturally visits a top-level `describe` before any
/// nested ones.
fn find_describe_call(node: Node, source: &[u8], found: &mut Option<(String, u32)>) {
    if found.is_some() {
        return;
    }
    if node.kind() == "call" {
        if let Some(method) = call_method_name(node, source) {
            if method == "describe" {
                let receiver_ok = match node.child_by_field_name("receiver") {
                    None => true,
                    Some(r) => r.utf8_text(source) == Ok("RSpec"),
                };
                if receiver_ok {
                    let args = call_args(node);
                    if let Some(first) = args.first() {
                        if let Some(name) = constant_text(*first, source) {
                            *found = Some((name, src_line(node, 0)));
                            return;
                        }
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_describe_call(child, source, found);
        if found.is_some() {
            return;
        }
    }
}

fn search_app_role_dirs(repo_root: &Path, const_name: &str) -> Vec<String> {
    let segments: Vec<String> = const_name.split("::").map(underscore_segment).collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Vec::new();
    }
    let suffix = format!("{}.rb", segments.join("/"));
    let mut out = Vec::new();
    for dir in APP_ROLE_DIRS {
        let candidate = format!("{dir}/{suffix}");
        if repo_root.join(&candidate).is_file() {
            out.push(candidate);
        }
    }
    out.sort();
    out
}

const PATH_CONVENTION_MAP: &[(&str, &str)] = &[
    ("models", "app/models"),
    ("controllers", "app/controllers"),
    ("helpers", "app/helpers"),
    ("jobs", "app/jobs"),
    ("mailers", "app/mailers"),
    ("components", "app/components"),
    ("services", "app/services"),
    ("lib", "lib"),
];

fn resolve_path_convention(path: &str, repo_root: &Path) -> Option<FrameworkEdge> {
    let rest = path.strip_prefix("spec/")?;
    let rest = rest.strip_suffix("_spec.rb")?;
    let (top, remainder) = rest.split_once('/')?;
    let app_dir = PATH_CONVENTION_MAP
        .iter()
        .find(|(k, _)| *k == top)
        .map(|(_, v)| *v)?;
    let candidate = format!("{app_dir}/{remainder}.rb");
    if !repo_root.join(&candidate).is_file() {
        return None;
    }
    make_subject_edge(path, None, vec![candidate], "path_convention")
}

fn make_subject_edge(
    path: &str,
    line: Option<u32>,
    candidates: Vec<String>,
    resolution: &str,
) -> Option<FrameworkEdge> {
    match candidates.len() {
        0 => None,
        1 => Some(FrameworkEdge {
            kind: EdgeKind::SpecSubject,
            src_path: path.to_string(),
            src_line: line,
            src_symbol: None,
            dst_kind: Some("source_file".to_string()),
            dst_path: Some(candidates.into_iter().next().unwrap()),
            dst_symbol: None,
            trust: Trust::Likely,
            extra_json: Some(format!(r#"{{"resolution":"{resolution}"}}"#)),
        }),
        _ => {
            let cj = candidates
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(",");
            Some(FrameworkEdge {
                kind: EdgeKind::SpecSubject,
                src_path: path.to_string(),
                src_line: line,
                src_symbol: None,
                dst_kind: Some("source_file".to_string()),
                dst_path: Some(candidates[0].clone()),
                dst_symbol: None,
                trust: Trust::Candidate,
                extra_json: Some(format!(
                    r#"{{"resolution":"{resolution}","candidates":[{cj}]}}"#
                )),
            })
        }
    }
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
    fn described_class_resolves_over_path_convention() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/models/round.rb", "class Round; end\n");
        let src = b"RSpec.describe Round do\n  it 'works' do; end\nend\n";
        let edges = extract(root, "spec/models/round_spec.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_path, Some("app/models/round.rb".to_string()));
        assert_eq!(edges[0].trust, Trust::Likely);
        assert!(edges[0]
            .extra_json
            .as_deref()
            .unwrap()
            .contains("described_class"));
    }

    #[test]
    fn bare_describe_without_rspec_prefix_also_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/controllers/rounds_controller.rb",
            "class RoundsController; end\n",
        );
        let src = b"describe RoundsController do\nend\n";
        let edges = extract(root, "spec/controllers/rounds_controller_spec.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/controllers/rounds_controller.rb".to_string())
        );
    }

    #[test]
    fn describe_with_a_string_description_falls_back_to_path_convention() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/models/round.rb", "class Round; end\n");
        let src = b"RSpec.describe 'some feature' do\nend\n";
        let edges = extract(root, "spec/models/round_spec.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_path, Some("app/models/round.rb".to_string()));
        assert!(edges[0]
            .extra_json
            .as_deref()
            .unwrap()
            .contains("path_convention"));
        assert_eq!(edges[0].src_line, None); // file-level, path-convention resolution
    }

    #[test]
    fn request_spec_directory_has_no_path_convention_mapping() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"RSpec.describe 'GET /rounds' do\nend\n";
        let edges = extract(root, "spec/requests/rounds_spec.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn no_matching_source_file_anywhere_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"RSpec.describe GhostModel do\nend\n";
        let edges = extract(root, "spec/models/ghost_model_spec.rb", src);
        assert!(edges.is_empty());
    }
}
