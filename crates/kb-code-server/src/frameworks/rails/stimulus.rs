//! PRR-N4 — Stimulus: `data-controller="a b"` / `data-action="evt->ctrl#
//! method"` attribute literals → JS controller files
//! (`EdgeKind::StimulusBinding`).
//!
//! # Scanning strategy: closed regex over ERB `content` nodes, no HTML
//! grammar
//!
//! Per design-nav.md §2's own ruling, this deliberately does NOT add
//! `tree-sitter-html`: Stimulus's `data-controller`/`data-action`
//! mini-grammar is itself closed and well-documented, so a small regex
//! over the embedded-template grammar's raw HTML `content` nodes (never
//! the `code`/`directive` nodes — those are Ruby, already `views.rs`'s and
//! this crate's other extractors' territory) is sufficient. Only DOUBLE-
//! quoted attribute values are matched (`data-controller="..."` — the
//! overwhelming ERB convention); single-quoted attributes are a documented,
//! accepted gap.
//!
//! # Identifier → path convention
//!
//! A Stimulus identifier's `--` separates NESTED-controller-directory
//! segments (`admin--foo` → `admin/foo_controller.js`); a `-` WITHIN a
//! segment maps to `_` (`catalog-upload` → `catalog_upload_controller.js`,
//! `hotwire-native-bridge--apple-auth-bridge` →
//! `hotwire_native_bridge/apple_auth_bridge_controller.js`).
//!
//! # Resolution: two-tier, bounded — never a full recursive repo walk
//!
//! The "textbook" convention is a single `app/javascript/controllers/`
//! tree, but real multi-pack Rails apps (verified against the actual
//! acme-shop repo) instead scope controllers PER PACK
//! (`app/javascript/trade/controllers/`, `app/javascript/checkout/
//! controllers/`, …) — there is no one fixed path that covers both. Rather
//! than a full recursive `app/javascript/**` walk on every attribute (real
//! cost risk on a disk-bound host re-resolving on every visit, per
//! `Store::replace_rails_edges`'s "always re-resolve" doc), resolution
//! tries: (1) the textbook `app/javascript/controllers/<suffix>` path, then
//! (2) `app/javascript/<pack>/controllers/<suffix>` for each TOP-LEVEL
//! directory under `app/javascript/` (one bounded, non-recursive
//! `read_dir` — cheap even for a dozen packs). A controller nested deeper
//! than one pack level is an accepted, documented miss (honest drop, never
//! fabricated).

use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::path::Path;
use std::sync::OnceLock;
use tree_sitter::Node;

fn data_controller_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#"data-controller\s*=\s*"([^"]*)""#).unwrap())
}

fn data_action_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#"data-action\s*=\s*"([^"]*)""#).unwrap())
}

/// `path` must already be gated to an ERB file (a view OR a component's
/// co-located template — `rails::is_erb_view_file`/`is_component_template_file`).
pub fn extract(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("erb", bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk_content_nodes(tree.root_node(), bytes, path, repo_root, &mut out);
    out
}

/// Walk every `content` node (raw HTML/text — never `code`/`directive`,
/// which are Ruby) and regex-scan its text.
fn walk_content_nodes(
    node: Node,
    source: &[u8],
    path: &str,
    repo_root: &Path,
    out: &mut Vec<FrameworkEdge>,
) {
    if node.kind() == "content" {
        if let Ok(text) = node.utf8_text(source) {
            let base_row = node.start_position().row as u32;
            scan_attributes(text, base_row, path, repo_root, out);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_content_nodes(child, source, path, repo_root, out);
    }
}

fn scan_attributes(
    text: &str,
    base_row: u32,
    path: &str,
    repo_root: &Path,
    out: &mut Vec<FrameworkEdge>,
) {
    for cap in data_controller_re().captures_iter(text) {
        let whole = cap.get(0).unwrap();
        let value = &cap[1];
        let line = base_row + text[..whole.start()].matches('\n').count() as u32 + 1;
        for ident in value.split_whitespace() {
            emit_stimulus_edge(ident, line, path, repo_root, out);
        }
    }
    for cap in data_action_re().captures_iter(text) {
        let whole = cap.get(0).unwrap();
        let value = &cap[1];
        let line = base_row + text[..whole.start()].matches('\n').count() as u32 + 1;
        for token in value.split_whitespace() {
            if let Some(ident) = controller_from_action_token(token) {
                emit_stimulus_edge(&ident, line, path, repo_root, out);
            }
        }
    }
}

/// A `data-action` token: `[event[@target]->]controller#method[:modifier]`
/// (the event/`@window`/`@document`/`:modifier` parts are all optional —
/// see Stimulus's own action-descriptor grammar). Returns the controller
/// identifier, or `None` if the token isn't `#`-shaped at all.
fn controller_from_action_token(token: &str) -> Option<String> {
    let after_arrow = token.rsplit("->").next().unwrap_or(token);
    let (ctrl_part, _rest) = after_arrow.split_once('#')?;
    if ctrl_part.is_empty() {
        None
    } else {
        Some(ctrl_part.to_string())
    }
}

fn emit_stimulus_edge(
    identifier: &str,
    line: u32,
    path: &str,
    repo_root: &Path,
    out: &mut Vec<FrameworkEdge>,
) {
    if identifier.is_empty() {
        return;
    }
    let matches = resolve_stimulus_controller(repo_root, identifier);
    match matches.len() {
        0 => {}
        1 => out.push(FrameworkEdge {
            kind: EdgeKind::StimulusBinding,
            src_path: path.to_string(),
            src_line: Some(line),
            src_symbol: Some(identifier.to_string()),
            dst_kind: Some("js_controller".to_string()),
            dst_path: Some(matches.into_iter().next().unwrap()),
            dst_symbol: None,
            trust: Trust::Likely,
            extra_json: None,
        }),
        _ => {
            let candidates = matches
                .iter()
                .map(|m| format!("\"{m}\""))
                .collect::<Vec<_>>()
                .join(",");
            out.push(FrameworkEdge {
                kind: EdgeKind::StimulusBinding,
                src_path: path.to_string(),
                src_line: Some(line),
                src_symbol: Some(identifier.to_string()),
                dst_kind: Some("js_controller".to_string()),
                dst_path: Some(matches[0].clone()),
                dst_symbol: None,
                trust: Trust::Candidate,
                extra_json: Some(format!(r#"{{"candidates":[{candidates}]}}"#)),
            });
        }
    }
}

/// `identifier` (e.g. `"admin--foo"`, `"catalog-upload"`) → the set of
/// EXISTING JS controller files matching it, per the module doc's two-tier
/// search. Sorted for deterministic output.
fn resolve_stimulus_controller(repo_root: &Path, identifier: &str) -> Vec<String> {
    let segments: Vec<String> = identifier
        .split("--")
        .map(|seg| seg.replace('-', "_"))
        .collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Vec::new();
    }
    let suffix = format!("{}_controller.js", segments.join("/"));

    let mut out = Vec::new();
    let textbook = format!("app/javascript/controllers/{suffix}");
    if repo_root.join(&textbook).is_file() {
        out.push(textbook);
    }
    let js_root = repo_root.join("app/javascript");
    if let Ok(entries) = std::fs::read_dir(&js_root) {
        let mut packs: Vec<_> = entries.flatten().collect();
        packs.sort_by_key(|e| e.file_name());
        for entry in packs {
            if !entry.path().is_dir() {
                continue;
            }
            let pack = entry.file_name();
            let pack = pack.to_string_lossy();
            if pack == "controllers" {
                continue; // already covered by the textbook path above
            }
            let candidate = format!("app/javascript/{pack}/controllers/{suffix}");
            if repo_root.join(&candidate).is_file() {
                out.push(candidate);
            }
        }
    }
    out.sort();
    out.dedup();
    out
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
    fn simple_data_controller_resolves_textbook_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/javascript/controllers/row_controller.js", "");
        let src = b"<div data-controller=\"row\"></div>\n";
        let edges = extract(root, "app/views/x/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::StimulusBinding);
        assert_eq!(edges[0].src_symbol.as_deref(), Some("row"));
        assert_eq!(
            edges[0].dst_path,
            Some("app/javascript/controllers/row_controller.js".to_string())
        );
        assert_eq!(edges[0].trust, Trust::Likely);
    }

    #[test]
    fn pack_scoped_controller_resolves_via_second_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/javascript/trade/controllers/catalog_upload_controller.js",
            "",
        );
        let src = b"<div data-controller=\"catalog-upload\"></div>\n";
        let edges = extract(root, "app/views/trade/rounds/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/javascript/trade/controllers/catalog_upload_controller.js".to_string())
        );
    }

    #[test]
    fn admin_namespace_convention_maps_double_dash_to_nested_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/javascript/controllers/admin/foo_controller.js",
            "",
        );
        let src = b"<div data-controller=\"admin--foo\"></div>\n";
        let edges = extract(root, "app/views/x/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/javascript/controllers/admin/foo_controller.js".to_string())
        );
    }

    #[test]
    fn multi_controller_attribute_emits_one_edge_each() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/javascript/controllers/a_controller.js", "");
        write(root, "app/javascript/controllers/b_controller.js", "");
        let src = b"<div data-controller=\"a b\"></div>\n";
        let edges = extract(root, "app/views/x/index.html.erb", src);
        assert_eq!(edges.len(), 2);
        let names: Vec<_> = edges.iter().filter_map(|e| e.src_symbol.clone()).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }

    #[test]
    fn data_action_extracts_controller_from_event_arrow_form() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/javascript/controllers/modal_controller.js", "");
        let src = b"<div data-action=\"click->modal#close\"></div>\n";
        let edges = extract(root, "app/views/x/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].src_symbol.as_deref(), Some("modal"));
    }

    #[test]
    fn data_action_bare_form_with_no_event_arrow_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/javascript/controllers/form_controller.js", "");
        let src = b"<form data-action=\"form#save\"></form>\n";
        let edges = extract(root, "app/views/x/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].src_symbol.as_deref(), Some("form"));
    }

    #[test]
    fn data_action_double_dash_namespace_and_modifier_suffix_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/javascript/controllers/bridge/rating_controller.js",
            "",
        );
        let src = b"<div data-action=\"click->bridge--rating#rate:prevent\"></div>\n";
        let edges = extract(root, "app/views/x/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/javascript/controllers/bridge/rating_controller.js".to_string())
        );
    }

    #[test]
    fn unresolvable_controller_is_dropped_not_fabricated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"<div data-controller=\"ghost\"></div>\n";
        let edges = extract(root, "app/views/x/index.html.erb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn ruby_code_inside_erb_tags_is_never_scanned_as_html_content() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/javascript/controllers/row_controller.js", "");
        // `data-controller` appearing only inside a Ruby STRING literal
        // inside a `<%= %>` tag must not be picked up by the content-node
        // walk (it's inside `code`, not `content`).
        let src = b"<%= link_to 'x', y, data: { controller: 'row' } %>\n";
        let edges = extract(root, "app/views/x/index.html.erb", src);
        assert!(edges.is_empty());
    }
}
