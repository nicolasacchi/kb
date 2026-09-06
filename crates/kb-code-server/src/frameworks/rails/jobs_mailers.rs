//! PRR-N4 — ActiveJob/ActionMailer call sites: `FooJob.perform_later`/
//! `.perform_now`/`.set(...).perform_later` (`EdgeKind::JobEnqueue`) and
//! `FooMailer.bar.deliver_later`/`.deliver_now` (`EdgeKind::MailerDeliver`).
//!
//! Both patterns are RECEIVER-CHAIN shapes rather than argument literals
//! (unlike `views.rs`'s `render`/`i18n`'s `t(...)`), so resolution walks
//! the call's `receiver` chain instead of its `arguments`:
//! - `FooJob.perform_later(...)`: receiver is (optionally, via `.set(...)`
//!   scheduling options) the job constant itself.
//! - `FooMailer.bar.deliver_later`: receiver is `FooMailer.bar` (a `call`
//!   whose OWN receiver is the mailer constant and whose method name is
//!   the mailer ACTION) — `bar` here is never independently resolved as a
//!   job/mailer call by this same walk (its method name is `bar`, not
//!   `perform_later`/`deliver_later`, so the shared `support::walk_calls`
//!   visiting it as its own `call` node is a harmless non-match).
//!
//! Trust: always `Trust::Likely`, but ONLY once the resolved job/mailer
//! class file is verified to exist on disk — never fabricated.

use crate::frameworks::rails::support::{
    call_method_name, const_to_path, constant_text, src_line, walk_calls, walk_erb_ruby_fragments,
};
use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::path::Path;
use tree_sitter::Node;

/// `path` is any Ruby file already dispatched here (controller, model,
/// job, or mailer — jobs/mailers can enqueue/deliver each other).
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
        &mut |node, source, offset| resolve_call(node, source, offset, path, repo_root),
    );
    out
}

/// `path` is an ERB file (view or component template) already dispatched
/// here.
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
                    resolve_call(node, source, line_offset, path, repo_root)
                },
            );
        },
    );
    out
}

/// V72-H3 — the `.haml` sibling of [`extract_from_erb`].
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
                    resolve_call(node, source, line_offset, path, repo_root)
                },
            );
        },
    );
    out
}

fn resolve_call(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    repo_root: &Path,
) -> Vec<FrameworkEdge> {
    let mut out = resolve_job_enqueue(node, source, line_offset, path, repo_root);
    out.extend(resolve_mailer_deliver(
        node,
        source,
        line_offset,
        path,
        repo_root,
    ));
    out
}

fn resolve_job_enqueue(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    repo_root: &Path,
) -> Vec<FrameworkEdge> {
    let Some(method) = call_method_name(node, source) else {
        return Vec::new();
    };
    if !matches!(method.as_str(), "perform_later" | "perform_now") {
        return Vec::new();
    }
    let Some(receiver) = node.child_by_field_name("receiver") else {
        return Vec::new();
    };
    let Some(job_const) = unwrap_scheduling_chain(receiver, source) else {
        return Vec::new();
    };
    let dst_path = const_to_path(&job_const, "app/jobs");
    if !repo_root.join(&dst_path).is_file() {
        return Vec::new();
    }
    vec![FrameworkEdge {
        kind: EdgeKind::JobEnqueue,
        src_path: path.to_string(),
        src_line: Some(src_line(node, line_offset)),
        src_symbol: None,
        dst_kind: Some("job_class".to_string()),
        dst_path: Some(dst_path),
        dst_symbol: None,
        trust: Trust::Likely,
        extra_json: Some(format!(r#"{{"method":"{method}"}}"#)),
    }]
}

/// Unwrap an ActiveJob scheduling chain (`X.set(...).perform_later` — the
/// `.set(...)` call's OWN receiver is walked through, repeatedly, until a
/// bare/namespaced constant is reached) down to the job class name.
fn unwrap_scheduling_chain(node: Node, source: &[u8]) -> Option<String> {
    let mut cur = node;
    loop {
        if let Some(name) = constant_text(cur, source) {
            return Some(name);
        }
        if cur.kind() != "call" {
            return None;
        }
        if call_method_name(cur, source).as_deref() != Some("set") {
            return None;
        }
        cur = cur.child_by_field_name("receiver")?;
    }
}

fn resolve_mailer_deliver(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    repo_root: &Path,
) -> Vec<FrameworkEdge> {
    let Some(method) = call_method_name(node, source) else {
        return Vec::new();
    };
    if !matches!(method.as_str(), "deliver_later" | "deliver_now") {
        return Vec::new();
    }
    let Some(action_call) = node.child_by_field_name("receiver") else {
        return Vec::new();
    };
    if action_call.kind() != "call" {
        return Vec::new();
    }
    let Some(action_name) = call_method_name(action_call, source) else {
        return Vec::new();
    };
    let Some(mailer_recv) = action_call.child_by_field_name("receiver") else {
        return Vec::new();
    };
    let Some(mailer_const) = constant_text(mailer_recv, source) else {
        return Vec::new();
    };
    let dst_path = const_to_path(&mailer_const, "app/mailers");
    if !repo_root.join(&dst_path).is_file() {
        return Vec::new();
    }
    vec![FrameworkEdge {
        kind: EdgeKind::MailerDeliver,
        src_path: path.to_string(),
        src_line: Some(src_line(node, line_offset)),
        src_symbol: Some(action_name.clone()),
        dst_kind: Some("mailer_action".to_string()),
        dst_path: Some(dst_path),
        dst_symbol: Some(action_name),
        trust: Trust::Likely,
        extra_json: Some(format!(r#"{{"method":"{method}"}}"#)),
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
    fn perform_later_resolves_job_class() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/jobs/foo_job.rb",
            "class FooJob < ApplicationJob; end\n",
        );
        let src = b"class X\n  def call\n    FooJob.perform_later(1)\n  end\nend\n";
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::JobEnqueue);
        assert_eq!(edges[0].dst_path, Some("app/jobs/foo_job.rb".to_string()));
        assert_eq!(edges[0].src_line, Some(3));
    }

    #[test]
    fn set_scheduling_chain_unwraps_to_the_job_class() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/jobs/foo_job.rb",
            "class FooJob < ApplicationJob; end\n",
        );
        let src = b"FooJob.set(wait: 5.minutes).perform_later(1)\n";
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_path, Some("app/jobs/foo_job.rb".to_string()));
    }

    #[test]
    fn job_without_a_matching_file_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"GhostJob.perform_later\n";
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn mailer_deliver_later_resolves_mailer_and_action() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/mailers/user_mailer.rb",
            "class UserMailer < ApplicationMailer; end\n",
        );
        let src = b"UserMailer.welcome(@user).deliver_later\n";
        let edges = extract_from_ruby(root, "app/controllers/x_controller.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::MailerDeliver);
        assert_eq!(
            edges[0].dst_path,
            Some("app/mailers/user_mailer.rb".to_string())
        );
        assert_eq!(edges[0].dst_symbol.as_deref(), Some("welcome"));
    }

    #[test]
    fn mailer_deliver_from_erb_tag_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/mailers/user_mailer.rb",
            "class UserMailer < ApplicationMailer; end\n",
        );
        let src = b"<% UserMailer.welcome(@user).deliver_now %>\n";
        let edges = extract_from_erb(root, "app/views/x/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_symbol.as_deref(), Some("welcome"));
    }

    #[test]
    fn unrelated_deliver_later_call_without_mailer_shape_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // receiver is a plain method call, not `Constant.action`.
        let src = b"some_object.deliver_later\n";
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert!(edges.is_empty());
    }
}
