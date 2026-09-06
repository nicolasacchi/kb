//! PRR-N3 — controller render/redirect analysis + ERB render-partial/
//! turbo_stream call-site scanning. Two entry points ([`extract_controller`]
//! and [`extract_erb`]) share one call-resolution core ([`resolve_render_call`])
//! over two [`RenderCtx`] flavors, because the SAME `render "x"` call means
//! different things depending on where it's written:
//!
//! - **In a controller action** (no `partial:`/`template:` key): Rails
//!   treats a bare string/symbol as `render action: "x"` — a FULL VIEW
//!   render (`RenderCtx::Controller`).
//! - **In an ERB view** (no `partial:`/`template:` key): a bare string is
//!   the well-known partial shorthand (`<%= render "form" %>` ==
//!   `<%= render partial: "form" %>`) — a PARTIAL (`RenderCtx::View`).
//!
//! `render partial: "x"`/`render template: "x"`/`render action: "x"` are
//! unambiguous in EITHER context (the explicit key always wins).
//!
//! # ERB per-tag Ruby injection (the accepted ceiling)
//!
//! `tree-sitter-embedded-template` gives tag boundaries (`directive`/
//! `output_directive`/`comment_directive` vs raw HTML `content`), not a
//! unified Ruby AST spanning multiple tags. [`extract_erb`] walks the ERB
//! tree, and for every `directive`/`output_directive` node re-parses its
//! `code` child's bytes as an INDEPENDENT Ruby fragment via
//! `tree-sitter-ruby`. This does NOT reconstruct control flow split across
//! tags (`<% if x %>...<% else %>...<% end %>`) — each fragment is scanned
//! for call sites on its own. `comment_directive` (`<%# … %>`) is never
//! scanned — that code never executes. See design-nav.md §2/§8 open
//! question 3 for the accepted-gap rationale.
//!
//! # Implicit `render_view` (controller actions with no explicit response)
//!
//! Real Rails semantics: an action falls through to the implicit default
//! template UNLESS it calls `render`/`redirect_to`/`redirect_back`/`head`/
//! `send_data`/`send_file`/`send_stream`/`respond_to` — and even then, only
//! on the code paths that DON'T take one of those branches. Determining
//! "does every branch respond explicitly" is full branch-coverage analysis,
//! out of scope. The SAFE heuristic used here: if the action body contains
//! **zero** such calls anywhere (regardless of branching), it definitely
//! falls through — emit `render_view` at `Trust::Likely`. If it contains
//! **any** such call anywhere, do NOT emit an implicit edge for that
//! action, even though some other branch might still fall through — a
//! false negative (a missed real edge) is honest; a false positive (a
//! render_view edge for an action that never actually falls through) would
//! not be.

use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::path::Path;
use tree_sitter::Node;

/// Where a `render`/`turbo_stream.*` call site lives — see the module doc.
#[derive(Debug, Clone)]
enum RenderCtx {
    Controller { controller_path: String },
    View { view_dir: String },
}

impl RenderCtx {
    /// The source-relative `app/views/...` directory a bare (no-slash)
    /// partial/view name resolves against.
    fn current_view_dir(&self) -> String {
        match self {
            RenderCtx::Controller { controller_path } => format!("app/views/{controller_path}"),
            RenderCtx::View { view_dir } => view_dir.clone(),
        }
    }
}

/// Response-terminating calls (bare, no receiver) whose PRESENCE anywhere
/// in an action body suppresses the implicit `render_view` edge — see the
/// module doc's heuristic. `render_to_string` is deliberately excluded: it
/// returns a string for further use, it doesn't itself respond.
const RESPONSE_CALL_NAMES: &[&str] = &[
    "render",
    "redirect_to",
    "redirect_back",
    "head",
    "send_data",
    "send_file",
    "send_stream",
    "respond_to",
];

// --- controller entry point --------------------------------------------------

/// `path` must already be gated to `app/controllers/**/*.rb` by the caller
/// (`rails::is_controller_file`). Controller identity is derived from the
/// FILE PATH convention (`app/controllers/trade/rounds_controller.rb` →
/// `trade/rounds`), not from parsing Ruby module/class nesting — simpler
/// and matches how `routes.rs` resolves the same identity from the
/// `namespace` DSL chain (both sides of the MVC triangle agree without
/// either needing to track the other's representation).
pub fn extract_controller(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("ruby", bytes) else {
        return Vec::new();
    };
    let Some(controller_path) = controller_identity(path) else {
        return Vec::new();
    };
    let Some(class_node) = find_first_class(tree.root_node()) else {
        return Vec::new();
    };
    let Some(body) = class_node.child_by_field_name("body") else {
        return Vec::new();
    };

    let ctx = RenderCtx::Controller {
        controller_path: controller_path.clone(),
    };
    let mut out = Vec::new();
    let mut private_from_here = false;
    let mut cursor = body.walk();
    for stmt in body.named_children(&mut cursor) {
        if is_visibility_switch(stmt, bytes) {
            private_from_here = true;
            continue;
        }
        if private_from_here {
            continue;
        }
        // `def self.x` (singleton_method) is a class method, never an
        // action — only plain `method` nodes count.
        if stmt.kind() != "method" {
            continue;
        }
        let Some(name_node) = stmt.child_by_field_name("name") else {
            continue;
        };
        let Ok(action_name) = name_node.utf8_text(bytes) else {
            continue;
        };

        match stmt.child_by_field_name("body") {
            Some(method_body) => {
                scan_calls(method_body, bytes, &ctx, path, repo_root, 0, &mut out);
                if !method_body_has_response_call(method_body, bytes) {
                    out.extend(implicit_render_view_edge(
                        path,
                        stmt,
                        &controller_path,
                        action_name,
                        repo_root,
                    ));
                }
            }
            // An empty `def x; end` body has no statements at all — always
            // falls through.
            None => out.extend(implicit_render_view_edge(
                path,
                stmt,
                &controller_path,
                action_name,
                repo_root,
            )),
        }
    }
    out
}

fn controller_identity(path: &str) -> Option<String> {
    let rest = path.strip_prefix("app/controllers/")?;
    let rest = rest.strip_suffix("_controller.rb")?;
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

fn find_first_class(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "class" {
        return Some(node);
    }
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        if let Some(found) = find_first_class(c) {
            return Some(found);
        }
    }
    None
}

/// `true` for a bare `private`/`protected` statement (identifier OR a
/// no-argument, no-receiver call) — the whole-body visibility switch.
/// `private :specific_method` (WITH arguments) is intentionally NOT
/// tracked per-method (an accepted, documented gap — see the module doc's
/// posture on false negatives over false positives): that method stays
/// visible to this extractor.
fn is_visibility_switch(node: Node, source: &[u8]) -> bool {
    let text = match node.kind() {
        "identifier" => node.utf8_text(source).ok(),
        "call"
            if node.child_by_field_name("receiver").is_none()
                && node.child_by_field_name("arguments").is_none() =>
        {
            node.child_by_field_name("method")
                .and_then(|m| m.utf8_text(source).ok())
        }
        _ => None,
    };
    matches!(text, Some("private") | Some("protected"))
}

fn method_body_has_response_call(node: Node, source: &[u8]) -> bool {
    if node.kind() == "call" && node.child_by_field_name("receiver").is_none() {
        if let Some(name) = call_method_name(node, source) {
            if RESPONSE_CALL_NAMES.contains(&name.as_str()) {
                return true;
            }
        }
    }
    let mut cursor = node.walk();
    let found = node
        .children(&mut cursor)
        .any(|c| method_body_has_response_call(c, source));
    found
}

fn implicit_render_view_edge(
    path: &str,
    def_node: Node,
    controller_path: &str,
    action_name: &str,
    repo_root: &Path,
) -> Option<FrameworkEdge> {
    let dir_rel = format!("app/views/{controller_path}");
    let matches = find_view_files(repo_root, &dir_rel, action_name);
    make_view_edge(
        EdgeKind::RenderView,
        path,
        src_line(def_node, 0),
        matches,
        "view",
    )
}

// --- ERB entry point ----------------------------------------------------------

/// `path` must already be gated to `app/views/**/*.erb` by the caller
/// (`rails::is_erb_view_file`).
pub fn extract_erb(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("erb", bytes) else {
        return Vec::new();
    };
    let Some(view_dir) = path.rfind('/').map(|i| path[..i].to_string()) else {
        return Vec::new();
    };
    let ctx = RenderCtx::View { view_dir };
    let mut out = Vec::new();
    walk_erb_template(tree.root_node(), bytes, &ctx, path, repo_root, &mut out);
    out
}

/// PRR-N3's ERB entry point's V72-H3 sibling: the same render/turbo_stream
/// resolution over a `.haml` view.
///
/// It differs from [`extract_erb`] in exactly one line — the WALK — and
/// deliberately so: `scan_calls`, `resolve_render_call`, `resolve_render`,
/// `find_view_files` and `make_view_edge` are all shared unchanged, so a
/// `render "shared/menu"` written in HAML resolves to the same file, with
/// the same `EdgeKind` and the same `Trust`, as one written in ERB. That
/// equivalence is pinned by a test (`erb_and_haml_mint_the_same_edges`).
pub fn extract_haml(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Some(view_dir) = path.rfind('/').map(|i| path[..i].to_string()) else {
        return Vec::new();
    };
    let ctx = RenderCtx::View { view_dir };
    let mut out = Vec::new();
    crate::frameworks::rails::support::walk_haml_ruby_fragments(
        bytes,
        &mut out,
        &mut |root, source, offset, out| {
            scan_calls(root, source, &ctx, path, repo_root, offset, out);
        },
    );
    out
}

/// Walk the ERB tree; at every `directive`/`output_directive` tag, re-parse
/// its `code` child as an independent Ruby fragment (see the module doc's
/// "ERB per-tag Ruby injection" section) and scan IT for call sites.
/// `comment_directive` (`<%# … %>`) is deliberately never matched — dead
/// code, never executed.
fn walk_erb_template(
    node: Node,
    source: &[u8],
    ctx: &RenderCtx,
    path: &str,
    repo_root: &Path,
    out: &mut Vec<FrameworkEdge>,
) {
    if matches!(node.kind(), "directive" | "output_directive") {
        if let Some(code_node) = find_code_child(node) {
            if let Ok(code_text) = code_node.utf8_text(source) {
                if let Ok((ruby_tree, _language)) = crate::lang::parse("ruby", code_text.as_bytes())
                {
                    // `code_node`'s row is ALREADY absolute within the whole
                    // `.erb` file (tree-sitter tracks position within the
                    // node's own tree, not relative to some ancestor) — the
                    // independent Ruby fragment's OWN nodes start counting
                    // from row 0 again, so this offset re-anchors them.
                    let offset = code_node.start_position().row as u32;
                    scan_calls(
                        ruby_tree.root_node(),
                        code_text.as_bytes(),
                        ctx,
                        path,
                        repo_root,
                        offset,
                        out,
                    );
                }
            }
        }
        // Do not descend further into a directive's own children here —
        // `scan_calls` above already walked the (independently re-parsed)
        // code fragment; the ERB-tree `code` node's raw bytes are not
        // themselves useful to re-visit as ERB nodes.
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_erb_template(child, source, ctx, path, repo_root, out);
    }
}

fn find_code_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let found = node.children(&mut cursor).find(|c| c.kind() == "code");
    found
}

// --- shared call-site scan + resolution ---------------------------------------

/// Recursively walk `node`'s WHOLE subtree (not just top-level statements —
/// a render call nested inside `if`/`respond_to`/`case` is still a real
/// call site) looking for `call` nodes this lens recognizes.
fn scan_calls(
    node: Node,
    source: &[u8],
    ctx: &RenderCtx,
    path: &str,
    repo_root: &Path,
    line_offset: u32,
    out: &mut Vec<FrameworkEdge>,
) {
    if node.kind() == "call" {
        out.extend(resolve_render_call(
            node,
            source,
            ctx,
            path,
            repo_root,
            line_offset,
        ));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        scan_calls(child, source, ctx, path, repo_root, line_offset, out);
    }
}

fn resolve_render_call(
    node: Node,
    source: &[u8],
    ctx: &RenderCtx,
    path: &str,
    repo_root: &Path,
    line_offset: u32,
) -> Vec<FrameworkEdge> {
    let Some(method) = call_method_name(node, source) else {
        return Vec::new();
    };
    let receiver_text = node
        .child_by_field_name("receiver")
        .and_then(|r| r.utf8_text(source).ok());

    match (receiver_text, method.as_str()) {
        (None, "render" | "render_to_string") => {
            resolve_render(node, source, ctx, path, repo_root, line_offset)
        }
        (
            Some("turbo_stream"),
            verb @ ("replace" | "append" | "prepend" | "update" | "remove" | "before" | "after"),
        ) => resolve_turbo_stream(node, source, verb, ctx, path, repo_root, line_offset),
        _ => Vec::new(),
    }
}

fn resolve_render(
    node: Node,
    source: &[u8],
    ctx: &RenderCtx,
    path: &str,
    repo_root: &Path,
    line_offset: u32,
) -> Vec<FrameworkEdge> {
    let args = call_args(node);
    let mut opts = Vec::new();
    let mut positional: Option<Node> = None;
    for a in &args {
        match a.kind() {
            "pair" => opts.push(*a),
            "hash" => collect_pairs_into(*a, &mut opts),
            _ => {
                if positional.is_none() {
                    positional = Some(*a);
                }
            }
        }
    }
    let line = src_line(node, line_offset);

    // Explicit type-changing keys always win, in EITHER context — and a
    // non-literal value under one of these keys is a hard drop (never fall
    // through to a different resolution rule).
    for (key, is_partial) in [("partial", true), ("template", false), ("action", false)] {
        if let Some(pair) = find_pair_node(&opts, key, source) {
            return match pair
                .child_by_field_name("value")
                .and_then(|v| literal_string_or_symbol(v, source))
            {
                Some(name) if is_partial => resolve_partial_name(&name, ctx, path, repo_root, line)
                    .into_iter()
                    .collect(),
                Some(name) => resolve_view_name(&name, ctx, path, repo_root, line)
                    .into_iter()
                    .collect(),
                None => Vec::new(), // non-literal — drop, don't guess.
            };
        }
    }

    // Bare positional form — meaning depends on `ctx` (see module doc).
    let Some(positional) = positional else {
        return Vec::new();
    };
    let Some(name) = literal_string_or_symbol(positional, source) else {
        return Vec::new();
    };
    match ctx {
        RenderCtx::View { .. } => resolve_partial_name(&name, ctx, path, repo_root, line)
            .into_iter()
            .collect(),
        RenderCtx::Controller { .. } => resolve_view_name(&name, ctx, path, repo_root, line)
            .into_iter()
            .collect(),
    }
}

fn resolve_turbo_stream(
    node: Node,
    source: &[u8],
    verb: &str,
    ctx: &RenderCtx,
    path: &str,
    repo_root: &Path,
    line_offset: u32,
) -> Vec<FrameworkEdge> {
    let args = call_args(node);
    let mut opts = Vec::new();
    let mut positional: Option<Node> = None;
    for a in &args {
        match a.kind() {
            "pair" => opts.push(*a),
            "hash" => collect_pairs_into(*a, &mut opts),
            _ => {
                if positional.is_none() {
                    positional = Some(*a);
                }
            }
        }
    }
    let line = src_line(node, line_offset);
    let mut out = Vec::new();

    if let Some(target) = positional.and_then(|p| literal_string_or_symbol(p, source)) {
        out.push(FrameworkEdge {
            kind: EdgeKind::TurboStreamTarget,
            src_path: path.to_string(),
            src_line: Some(line),
            src_symbol: None,
            dst_kind: Some("dom_id".to_string()),
            dst_path: None,
            dst_symbol: Some(target),
            trust: Trust::Likely,
            extra_json: Some(format!(r#"{{"verb":"{verb}"}}"#)),
        });
    }
    if let Some(pair) = find_pair_node(&opts, "partial", source) {
        if let Some(name) = pair
            .child_by_field_name("value")
            .and_then(|v| literal_string_or_symbol(v, source))
        {
            out.extend(resolve_partial_name(&name, ctx, path, repo_root, line));
        }
    }
    out
}

/// `name` may contain a `/` (cross-directory reference, e.g.
/// `"trade/shared/flash"` → `app/views/trade/shared/_flash.*`) or not (same
/// directory as `ctx.current_view_dir()`).
fn resolve_partial_name(
    name: &str,
    ctx: &RenderCtx,
    path: &str,
    repo_root: &Path,
    line: u32,
) -> Option<FrameworkEdge> {
    let (dir_rel, base) = split_name(name, ctx);
    let stem = format!("_{base}");
    let matches = find_view_files(repo_root, &dir_rel, &stem);
    make_view_edge(EdgeKind::RenderPartial, path, line, matches, "partial")
}

fn resolve_view_name(
    name: &str,
    ctx: &RenderCtx,
    path: &str,
    repo_root: &Path,
    line: u32,
) -> Option<FrameworkEdge> {
    let (dir_rel, base) = split_name(name, ctx);
    let matches = find_view_files(repo_root, &dir_rel, &base);
    make_view_edge(EdgeKind::RenderView, path, line, matches, "view")
}

/// Split a `render`-style name into `(source-relative app/views directory,
/// base filename stem)`.
fn split_name(name: &str, ctx: &RenderCtx) -> (String, String) {
    match name.rfind('/') {
        Some(idx) => (
            format!("app/views/{}", &name[..idx]),
            name[idx + 1..].to_string(),
        ),
        None => (ctx.current_view_dir(), name.to_string()),
    }
}

/// List files directly under `repo_root/dir_rel` whose filename (the
/// portion BEFORE the first `.`) equals `stem` exactly — e.g. `stem =
/// "_row"` matches `_row.html.erb` and `_row.turbo_stream.erb` both (an
/// intentional ambiguity: two format variants of the same partial name is
/// exactly the "multiple plausible targets" case `Trust::Candidate` exists
/// for). Sorted for deterministic golden output.
fn find_view_files(repo_root: &Path, dir_rel: &str, stem: &str) -> Vec<String> {
    let dir = repo_root.join(dir_rel);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let fname = fname.to_string_lossy().to_string();
        if fname.split('.').next() == Some(stem) {
            out.push(format!("{dir_rel}/{fname}"));
        }
    }
    out.sort();
    out
}

fn make_view_edge(
    kind: EdgeKind,
    path: &str,
    line: u32,
    matches: Vec<String>,
    dst_kind: &'static str,
) -> Option<FrameworkEdge> {
    match matches.len() {
        0 => None,
        1 => Some(FrameworkEdge {
            kind,
            src_path: path.to_string(),
            src_line: Some(line),
            src_symbol: None,
            dst_kind: Some(dst_kind.to_string()),
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
            Some(FrameworkEdge {
                kind,
                src_path: path.to_string(),
                src_line: Some(line),
                src_symbol: None,
                dst_kind: Some(dst_kind.to_string()),
                dst_path: Some(matches[0].clone()),
                dst_symbol: None,
                trust: Trust::Candidate,
                extra_json: Some(format!(r#"{{"candidates":[{candidates}]}}"#)),
            })
        }
    }
}

// --- shared Ruby AST helpers (small, deliberately NOT shared with
// `routes.rs` — see this module's PR report for the duplication tradeoff) --

fn src_line(node: Node, line_offset: u32) -> u32 {
    node.start_position().row as u32 + line_offset + 1
}

fn call_method_name(node: Node, source: &[u8]) -> Option<String> {
    let m = node.child_by_field_name("method")?;
    m.utf8_text(source).ok().map(|s| s.to_string())
}

fn call_args(node: Node<'_>) -> Vec<Node<'_>> {
    let Some(list) = node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = list.walk();
    list.named_children(&mut cursor).collect()
}

fn collect_pairs_into<'a>(node: Node<'a>, opts: &mut Vec<Node<'a>>) {
    if node.kind() == "hash" {
        let mut cursor = node.walk();
        for c in node.named_children(&mut cursor) {
            if c.kind() == "pair" {
                opts.push(c);
            }
        }
    }
}

fn pair_key_name(pair: Node, source: &[u8]) -> Option<String> {
    let key = pair.child_by_field_name("key")?;
    match key.kind() {
        "hash_key_symbol" => key
            .utf8_text(source)
            .ok()
            .map(|s| s.trim_end_matches(':').to_string()),
        "simple_symbol" => literal_symbol(key, source),
        "string" => literal_string(key, source),
        "identifier" => key.utf8_text(source).ok().map(|s| s.to_string()),
        _ => None,
    }
}

fn find_pair_node<'a>(opts: &[Node<'a>], key: &str, source: &[u8]) -> Option<Node<'a>> {
    opts.iter()
        .find(|p| pair_key_name(**p, source).as_deref() == Some(key))
        .copied()
}

fn literal_string_or_symbol(node: Node, source: &[u8]) -> Option<String> {
    literal_string(node, source).or_else(|| literal_symbol(node, source))
}

fn literal_string(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    if node
        .children(&mut cursor)
        .any(|c| c.kind() == "interpolation")
    {
        return None;
    }
    let text = node.utf8_text(source).ok()?;
    if text.len() < 2 {
        return None;
    }
    Some(text[1..text.len() - 1].to_string())
}

fn literal_symbol(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "simple_symbol" => {
            let text = node.utf8_text(source).ok()?;
            Some(text.trim_start_matches(':').to_string())
        }
        "delimited_symbol" => {
            let mut cursor = node.walk();
            if node
                .children(&mut cursor)
                .any(|c| c.kind() == "interpolation")
            {
                return None;
            }
            let text = node.utf8_text(source).ok()?;
            let after_colon = text.strip_prefix(':')?;
            if after_colon.len() < 2 {
                return None;
            }
            Some(after_colon[1..after_colon.len() - 1].to_string())
        }
        _ => None,
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

    // --- controller: implicit render_view -----------------------------------

    #[test]
    fn action_with_no_response_call_gets_implicit_render_view() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/views/trade/rounds/show.html.erb", "<p>hi</p>\n");
        let src = b"class Trade::RoundsController < ApplicationController\n  def show\n    @round = Round.find(params[:id])\n  end\nend\n";
        let edges = extract_controller(root, "app/controllers/trade/rounds_controller.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::RenderView);
        assert_eq!(
            edges[0].dst_path,
            Some("app/views/trade/rounds/show.html.erb".to_string())
        );
        assert_eq!(edges[0].trust, Trust::Likely);
    }

    #[test]
    fn action_with_a_render_call_gets_no_implicit_edge() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/views/trade/rounds/new.html.erb", "<p>hi</p>\n");
        let src = b"class Trade::RoundsController < ApplicationController\n  def create\n    render :new, status: :unprocessable_content\n  end\nend\n";
        let edges = extract_controller(root, "app/controllers/trade/rounds_controller.rb", src);
        // Only the explicit `render :new` edge — no implicit `create`-named
        // one (there's no create.html.erb to find anyway).
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/views/trade/rounds/new.html.erb".to_string())
        );
    }

    #[test]
    fn private_methods_are_never_treated_as_actions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/views/x/helper_method.html.erb", "hi\n");
        let src = b"class XController < ApplicationController\n  def index\n    redirect_to root_path\n  end\n\n  private\n\n  def helper_method\n  end\nend\n";
        let edges = extract_controller(root, "app/controllers/x_controller.rb", src);
        assert!(
            edges.is_empty(),
            "private method must not produce an implicit edge: {edges:?}"
        );
    }

    #[test]
    fn explicit_render_partial_from_a_controller_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/views/trade/rounds/_offers_tab_content.html.erb",
            "hi\n",
        );
        let src = br#"
class Trade::RoundsController < ApplicationController
  def offers_tab
    render partial: 'offers_tab_content', layout: false
  end
end
"#;
        let edges = extract_controller(root, "app/controllers/trade/rounds_controller.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::RenderPartial);
        assert_eq!(
            edges[0].dst_path,
            Some("app/views/trade/rounds/_offers_tab_content.html.erb".to_string())
        );
    }

    #[test]
    fn bare_render_symbol_in_controller_is_a_view_not_a_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/views/trade/rounds/edit.html.erb", "hi\n");
        let src = b"class Trade::RoundsController < ApplicationController\n  def update\n    render :edit, status: :unprocessable_content\n  end\nend\n";
        let edges = extract_controller(root, "app/controllers/trade/rounds_controller.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::RenderView);
        assert_eq!(
            edges[0].dst_path,
            Some("app/views/trade/rounds/edit.html.erb".to_string())
        );
    }

    #[test]
    fn non_literal_render_argument_is_dropped_not_fabricated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"class XController < ApplicationController\n  def show\n    render partial: computed_partial_name\n  end\nend\n";
        let edges = extract_controller(root, "app/controllers/x_controller.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn ambiguous_partial_across_two_prefixes_is_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/views/a/_row.html.erb", "a\n");
        write(root, "app/views/a/_row.turbo_stream.erb", "a2\n");
        let src = b"class AController < ApplicationController\n  def index\n    render partial: 'row'\n  end\nend\n";
        let edges = extract_controller(root, "app/controllers/a_controller.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].trust, Trust::Candidate);
        assert!(edges[0]
            .extra_json
            .as_deref()
            .unwrap()
            .contains("_row.html.erb"));
    }

    // --- ERB call-site scanning ----------------------------------------------

    #[test]
    fn erb_bare_render_string_is_a_partial_same_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/views/trade/rounds/_offers_tab_content.html.erb",
            "hi\n",
        );
        let src = b"<%= render \"offers_tab_content\" %>\n";
        let edges = extract_erb(
            root,
            "app/views/trade/rounds/upload_orders_complete.turbo_stream.erb",
            src,
        );
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::RenderPartial);
        assert_eq!(
            edges[0].dst_path,
            Some("app/views/trade/rounds/_offers_tab_content.html.erb".to_string())
        );
    }

    #[test]
    fn erb_cross_directory_partial_reference_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/views/trade/shared/_flash.html.erb", "hi\n");
        let src = b"<%= render \"trade/shared/flash\" %>\n";
        let edges = extract_erb(root, "app/views/trade/rounds/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0].dst_path,
            Some("app/views/trade/shared/_flash.html.erb".to_string())
        );
    }

    #[test]
    fn erb_view_component_render_is_non_literal_and_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let src = b"<%= render Trade::OrderRecapTableComponent.new(recap_items: @items) %>\n";
        let edges = extract_erb(root, "app/views/trade/rounds/_order_recap.html.erb", src);
        assert!(
            edges.is_empty(),
            "a ViewComponent .new(...) call must never fabricate a partial edge: {edges:?}"
        );
    }

    #[test]
    fn erb_turbo_stream_replace_with_partial_emits_both_edges() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "app/views/trade/rounds/_offers_tab_content.html.erb",
            "hi\n",
        );
        let src = b"<%= turbo_stream.replace \"trade_round_offers_tab\" do %>\n  <%= render \"offers_tab_content\" %>\n<% end %>\n";
        let edges = extract_erb(
            root,
            "app/views/trade/rounds/upload_orders_complete.turbo_stream.erb",
            src,
        );
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|e| e.kind == EdgeKind::TurboStreamTarget
            && e.dst_symbol.as_deref() == Some("trade_round_offers_tab")));
        assert!(edges.iter().any(|e| e.kind == EdgeKind::RenderPartial
            && e.dst_path.as_deref()
                == Some("app/views/trade/rounds/_offers_tab_content.html.erb")));
    }

    #[test]
    fn erb_line_numbers_account_for_multiline_tags() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/views/x/_row.html.erb", "hi\n");
        let src = b"<p>line1</p>\n<%=\n  render(\n    \"row\"\n  )\n%>\n";
        let edges = extract_erb(root, "app/views/x/index.html.erb", src);
        assert_eq!(edges.len(), 1);
        // The `render(` call itself starts on line 3 (1-based): line1 (1),
        // blank tag-open (2), `render(` (3).
        assert_eq!(edges[0].src_line, Some(3));
    }

    #[test]
    fn erb_comment_directive_is_never_scanned() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "app/views/x/_row.html.erb", "hi\n");
        let src = b"<%# render \"row\" %>\n";
        let edges = extract_erb(root, "app/views/x/index.html.erb", src);
        assert!(edges.is_empty());
    }
}
