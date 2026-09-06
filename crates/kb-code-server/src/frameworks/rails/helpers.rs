//! PRR-N4 — file-level controller ↔ helper convention edge
//! (`EdgeKind::HelperFor`).
//!
//! No per-call-site resolution: Rails mixes EVERY helper module into
//! EVERY view by default (`config.action_controller.include_all_helpers`,
//! true unless an app opts out), so there is no honest way to attribute a
//! SPECIFIC helper-method call site inside a view to ONE helper file — see
//! design-addendum-2.md §G's own "no per-call-site resolution (global
//! mixin ambiguity — recorded)" note. This extractor instead emits the
//! single, unambiguous FILE-LEVEL convention: `UsersController` ↔
//! `app/helpers/users_helper.rb` (namespaced controllers keep their full
//! path: `Trade::RoundsController` ↔ `app/helpers/trade/rounds_helper.rb`)
//! — mirroring `EdgeKind::RouteFile`'s file-level shape (`src_line: None`).

use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::path::Path;

/// `path` must already be gated to `app/controllers/**/*.rb`
/// (`rails::is_controller_file`).
pub fn extract_helper_for(repo_root: &Path, path: &str) -> Vec<FrameworkEdge> {
    let Some(controller_path) = controller_identity(path) else {
        return Vec::new();
    };
    let dst_path = format!("app/helpers/{controller_path}_helper.rb");
    if !repo_root.join(&dst_path).is_file() {
        return Vec::new();
    }
    vec![FrameworkEdge {
        kind: EdgeKind::HelperFor,
        src_path: path.to_string(),
        src_line: None,
        src_symbol: None,
        dst_kind: Some("helper".to_string()),
        dst_path: Some(dst_path),
        dst_symbol: None,
        trust: Trust::Likely,
        extra_json: None,
    }]
}

/// Same convention `views.rs::controller_identity` uses — duplicated here
/// (a five-line pure string function, not worth threading through
/// `support.rs` for one caller) rather than made `pub(crate)` on the
/// existing private copy.
fn controller_identity(path: &str) -> Option<String> {
    let rest = path.strip_prefix("app/controllers/")?;
    let rest = rest.strip_suffix("_controller.rb")?;
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
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
    fn helper_for_resolves_when_the_helper_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/helpers/users_helper.rb",
            "module UsersHelper; end\n",
        );
        let edges = extract_helper_for(root, "app/controllers/users_controller.rb");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::HelperFor);
        assert_eq!(edges[0].src_line, None);
        assert_eq!(
            edges[0].dst_path,
            Some("app/helpers/users_helper.rb".to_string())
        );
        assert_eq!(edges[0].trust, Trust::Likely);
    }

    #[test]
    fn namespaced_controller_keeps_its_full_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/helpers/trade/rounds_helper.rb",
            "module Trade; module RoundsHelper; end; end\n",
        );
        let edges = extract_helper_for(root, "app/controllers/trade/rounds_controller.rb");
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/helpers/trade/rounds_helper.rb".to_string())
        );
    }

    #[test]
    fn no_helper_file_emits_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let edges = extract_helper_for(root, "app/controllers/orphan_controller.rb");
        assert!(edges.is_empty());
    }
}
