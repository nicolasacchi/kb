//! PRR-N4 — ViewComponent: `render(FooComponent.new(...))` call-site
//! resolution + component↔template co-location (`EdgeKind::
//! ViewComponentRender`).
//!
//! # Two shapes, one kind
//!
//! - **Call site** (`extract_from_ruby`/`extract_from_erb`): `render(...)`
//!   with NO explicit `partial:`/`template:` key (those are `views.rs`'s
//!   territory — a disjoint call shape, see `views.rs::resolve_render`)
//!   whose bare positional argument is itself a `.new(...)` call on a Ruby
//!   constant (`FooComponent.new(...)`, `Trade::RowComponent.new(...)`).
//!   `views.rs`'s OWN
//!   `erb_view_component_render_is_non_literal_and_dropped` test already
//!   documents this exact shape being a deliberate non-match for its
//!   partial resolution — this module is where it resolves instead.
//!   `dst_kind = "component_class"`.
//! - **Co-location** (`extract_component_class`): a component `.rb` file →
//!   its sibling template sharing the SAME stem (`foo_component.rb` ↔
//!   `foo_component.html.erb`) — a FILE-LEVEL edge (`src_line: None`,
//!   mirrors `EdgeKind::RouteFile`'s own file-level shape).
//!   `dst_kind = "component_template"`.
//!
//! # Trust
//!
//! Every call-site edge is verified against the LIVE filesystem
//! (`repo_root.join(dst_path).is_file()`) before being emitted — a
//! `SomeObject.new(...)` positional `render` argument is ALSO the shape a
//! plain non-ViewComponent Renderable would use, so the existence check is
//! what keeps this lane honest rather than a hard-coded `*Component` name
//! requirement (never fabricate a target — the same law `views.rs`'s
//! `find_view_files` enforces). Co-location ambiguity (more than one
//! sibling format variant, e.g. `.html.erb` + `.turbo_stream.erb`) is
//! `Trust::Candidate`, mirroring `views::make_view_edge`'s identical rule.

use crate::frameworks::rails::support::{
    call_args, call_method_name, const_to_path, constant_text, src_line, walk_calls,
    walk_erb_ruby_fragments,
};
use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::path::Path;
use tree_sitter::Node;

/// `path` must already be gated to `app/components/**/*.rb`
/// (`rails::is_component_ruby_file`).
pub fn extract_component_class(repo_root: &Path, path: &str, _bytes: &[u8]) -> Vec<FrameworkEdge> {
    // Co-location is pure path convention — no Ruby parse needed.
    let (dir_rel, stem) = match path.rfind('/') {
        Some(idx) => (&path[..idx], &path[idx + 1..]),
        None => ("", path),
    };
    let Some(stem) = stem.strip_suffix(".rb") else {
        return Vec::new();
    };
    let matches = find_sibling_templates(repo_root, dir_rel, stem);
    match matches.len() {
        0 => Vec::new(),
        1 => vec![FrameworkEdge {
            kind: EdgeKind::ViewComponentRender,
            src_path: path.to_string(),
            src_line: None,
            src_symbol: None,
            dst_kind: Some("component_template".to_string()),
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
                kind: EdgeKind::ViewComponentRender,
                src_path: path.to_string(),
                src_line: None,
                src_symbol: None,
                dst_kind: Some("component_template".to_string()),
                dst_path: Some(matches[0].clone()),
                dst_symbol: None,
                trust: Trust::Candidate,
                extra_json: Some(format!(r#"{{"candidates":[{candidates}]}}"#)),
            }]
        }
    }
}

/// Sibling files directly under `repo_root/dir_rel` whose stem (portion
/// before the first `.`) equals `stem`, EXCLUDING the `.rb` file itself —
/// same directory-listing shape as `views::find_view_files`.
fn find_sibling_templates(repo_root: &Path, dir_rel: &str, stem: &str) -> Vec<String> {
    let dir = repo_root.join(dir_rel);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy().to_string();
        if fname.ends_with(".rb") {
            continue; // the component class file itself
        }
        if fname.split('.').next() == Some(stem) {
            let rel = if dir_rel.is_empty() {
                fname
            } else {
                format!("{dir_rel}/{fname}")
            };
            out.push(rel);
        }
    }
    out.sort();
    out
}

/// `path` is any Ruby file already dispatched here (controller, model,
/// job, mailer, or a component's own class file) — call sites can appear
/// anywhere Ruby executes.
pub fn extract_from_ruby(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("ruby", bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_calls(
        tree.root_node(),
        bytes,
        0,
        &mut out,
        &mut |node, source, offset| {
            resolve_render_component_call(node, source, offset, path, repo_root)
        },
    );
    out
}

/// `path` is any ERB file already dispatched here (a view or a component's
/// own co-located template).
pub fn extract_from_erb(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("erb", bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_erb_ruby_fragments(
        tree.root_node(),
        bytes,
        &mut out,
        &mut |root, source, offset, out| {
            walk_calls(
                root,
                source,
                offset,
                out,
                &mut |node, source, line_offset| {
                    resolve_render_component_call(node, source, line_offset, path, repo_root)
                },
            );
        },
    );
    out
}

/// V72-H3 — the `.haml` sibling of [`extract_from_erb`]. Same
/// `resolve_component_render`, same constant→path convention.
pub fn extract_from_haml(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let mut out = Vec::new();
    crate::frameworks::rails::support::walk_haml_ruby_fragments(
        bytes,
        &mut out,
        &mut |root, source, offset, out| {
            walk_calls(
                root,
                source,
                offset,
                out,
                &mut |node, source, line_offset| {
                    resolve_render_component_call(node, source, line_offset, path, repo_root)
                },
            );
        },
    );
    out
}

fn resolve_render_component_call(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    repo_root: &Path,
) -> Vec<FrameworkEdge> {
    let Some(method) = call_method_name(node, source) else {
        return Vec::new();
    };
    if method != "render" || node.child_by_field_name("receiver").is_some() {
        return Vec::new();
    }
    let args = call_args(node);
    let Some(first) = args.first().copied() else {
        return Vec::new();
    };
    if first.kind() != "call" || call_method_name(first, source).as_deref() != Some("new") {
        return Vec::new();
    }
    let Some(receiver) = first.child_by_field_name("receiver") else {
        return Vec::new();
    };
    let Some(const_name) = constant_text(receiver, source) else {
        return Vec::new();
    };
    let dst_path = const_to_path(&const_name, "app/components");
    if !repo_root.join(&dst_path).is_file() {
        return Vec::new(); // never fabricate — no verified target on disk.
    }
    vec![FrameworkEdge {
        kind: EdgeKind::ViewComponentRender,
        src_path: path.to_string(),
        src_line: Some(src_line(node, line_offset)),
        src_symbol: None,
        dst_kind: Some("component_class".to_string()),
        dst_path: Some(dst_path),
        dst_symbol: None,
        trust: Trust::Likely,
        extra_json: None,
    }]
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
    fn co_location_resolves_unique_sibling_template() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/components/row_component.rb",
            "class RowComponent; end\n",
        );
        write(root, "app/components/row_component.html.erb", "<p>hi</p>\n");
        let edges = extract_component_class(root, "app/components/row_component.rb", b"");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].src_line, None);
        assert_eq!(edges[0].dst_kind.as_deref(), Some("component_template"));
        assert_eq!(
            edges[0].dst_path,
            Some("app/components/row_component.html.erb".to_string())
        );
        assert_eq!(edges[0].trust, Trust::Likely);
    }

    #[test]
    fn co_location_absent_template_emits_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/components/row_component.rb",
            "class RowComponent; end\n",
        );
        let edges = extract_component_class(root, "app/components/row_component.rb", b"");
        assert!(edges.is_empty());
    }

    #[test]
    fn co_location_two_format_variants_is_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/components/row_component.rb", "");
        write(root, "app/components/row_component.html.erb", "a\n");
        write(root, "app/components/row_component.turbo_stream.erb", "b\n");
        let edges = extract_component_class(root, "app/components/row_component.rb", b"");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].trust, Trust::Candidate);
    }

    #[test]
    fn render_component_new_call_site_resolves_from_ruby() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/components/row_component.rb",
            "class RowComponent; end\n",
        );
        let src = b"class XController < ApplicationController\n  def show\n    render(RowComponent.new(item: @item))\n  end\nend\n";
        let edges = extract_from_ruby(root, "app/controllers/x_controller.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_kind.as_deref(), Some("component_class"));
        assert_eq!(
            edges[0].dst_path,
            Some("app/components/row_component.rb".to_string())
        );
        assert_eq!(edges[0].src_line, Some(3));
    }

    #[test]
    fn namespaced_component_constant_resolves_from_erb() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/components/trade/row_component.rb",
            "module Trade; class RowComponent; end; end\n",
        );
        let src = b"<%= render(Trade::RowComponent.new(item: item)) %>\n";
        let edges = extract_from_erb(root, "app/views/trade/rounds/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/components/trade/row_component.rb".to_string())
        );
    }

    #[test]
    fn render_component_new_without_a_matching_file_is_dropped_not_fabricated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"<%= render(GhostComponent.new) %>\n";
        let edges = extract_from_erb(root, "app/views/x/index.html.erb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn plain_partial_render_is_never_matched_here() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"<%= render partial: 'row' %>\n";
        let edges = extract_from_erb(root, "app/views/x/index.html.erb", src);
        assert!(edges.is_empty());
    }
}
