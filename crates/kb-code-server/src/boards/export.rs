//! V74-L1 — the three board exports: Markdown, JSON Canvas 1.0, kb-html.
//!
//! D10 lists them as a Track L MUST ("exports (md, JSON Canvas, kb-html)").
//! All three are pure functions of an already-RESOLVED board
//! ([`super::resolve::BoardOut`]) — they run no query, resolve no anchor and
//! read no file, so an export can never disagree with the board it came
//! from.
//!
//! # An export is a SNAPSHOT
//!
//! Every format carries that sentence. A board is re-resolved on every
//! read; an exported file is not, so a node that reads `pinned` in an
//! export may be an orphan five minutes later. Saying so in the artifact
//! is the difference between a snapshot and a lie.
//!
//! # JSON Canvas 1.0's one real cost
//!
//! The spec (<https://jsoncanvas.org/spec/1.0/>) requires `x`/`y`/`width`/
//! `height` on every node, and has no concept of a code RANGE, a blob, a
//! trust class or a resolution state. So the export is lossy in a stated
//! way: geometry is DERIVED by [`super::layout`] for this format only (see
//! that module's own doc for why the server otherwise owns no layout), and
//! everything the spec cannot express rides along in `kbc_`-prefixed
//! extension fields, which the spec's own extensibility rule says other
//! apps ignore. Round-tripping through another canvas app WILL lose them;
//! `kb-html` is the high-fidelity export.
//!
//! # kb-html and the authored-content posture
//!
//! This is the ONE place in kb-code that emits HTML (`spa::serve` ships a
//! pre-built bundle; `annotations::synthetic_html` builds an intermediate
//! nothing serves). Three rules make that safe, and each has a test:
//!
//! 1. **Every interpolated byte goes through [`escape_text`] or
//!    [`escape_attr`]** — kb-core's `session_render::esc`/`push_attr` shape,
//!    `&` first, in one pass, with `"` additionally escaped for attributes.
//! 2. **No `<script>`, no `<style>` with content this daemon did not write,
//!    no event-handler attribute, no `javascript:`/`data:` URL.** A `link`
//!    node's URL is already refused at lint time unless it is http(s).
//! 3. **This exporter authors no prose.** It assembles stored fields —
//!    exactly the posture `review_github_export::compose_comment_body`
//!    takes for its Markdown, and the reason `review_distill`'s "the agent
//!    authors the artifact" rule is not violated: an agent still decides
//!    whether the export becomes a kb artifact, and the
//!    `<template id="kb-prompt">` wrapper is NOT generated here (the kb
//!    side owns it — root CLAUDE.md's authoring contract).

use super::resolve::{BoardOut, NodeOut};
use super::*;

pub const FORMAT_MD: &str = "md";
pub const FORMAT_JSONCANVAS: &str = "jsoncanvas";
pub const FORMAT_KB_HTML: &str = "kb-html";

/// The CLOSED export vocabulary, in the order `--format`'s help lists it.
pub const FORMATS: [&str; 3] = [FORMAT_MD, FORMAT_JSONCANVAS, FORMAT_KB_HTML];

/// The sentence every format carries.
pub const SNAPSHOT_CAPTION: &str =
    "This is a SNAPSHOT. A kb-code board re-resolves every node against the working \
     tree on each read; this file does not. Its states were true when it was exported.";

pub fn is_valid_format(s: &str) -> bool {
    FORMATS.contains(&s)
}

/// The `Content-Type` a format is served with.
pub fn content_type(format: &str) -> &'static str {
    match format {
        FORMAT_JSONCANVAS => "application/json; charset=utf-8",
        FORMAT_KB_HTML => "text/html; charset=utf-8",
        _ => "text/markdown; charset=utf-8",
    }
}

/// The filename a `--out`-less CLI writes, and the
/// `Content-Disposition` name.
pub fn filename(slug: &str, format: &str) -> String {
    match format {
        FORMAT_JSONCANVAS => format!("{slug}.canvas"),
        FORMAT_KB_HTML => format!("{slug}.html"),
        _ => format!("{slug}.md"),
    }
}

// --- escaping --------------------------------------------------------------

/// HTML TEXT escaping — `&` first, one pass (kb-core's
/// `session_render::esc`; its `esc_is_single_pass_amp_first` test states
/// why the order is load-bearing: escaping `<` first would turn it into
/// `&amp;lt;`).
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// HTML ATTRIBUTE escaping — [`escape_text`] plus `"` and `'`. Split from
/// the text form for kb-core's own reason: an attribute has a delimiter a
/// text node does not.
pub fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;").replace('\'', "&#39;")
}

/// A caller-supplied absolute URL prefix for the reader links an export
/// emits, or the empty string for root-relative ones.
///
/// Validated rather than trusted: http(s) only, no whitespace, no quote, no
/// backslash, no `<`/`>`. An export is a file a human opens; a `base` that
/// could carry a `javascript:` scheme or break out of an attribute would be
/// a stored-XSS vector in a document this daemon signs its name to.
pub fn validate_base(base: &str) -> Result<String, String> {
    let b = base.trim_end_matches('/');
    if b.is_empty() {
        return Ok(String::new());
    }
    if !(b.starts_with("https://") || b.starts_with("http://")) {
        return Err(format!(
            "base {base:?} must be an http(s) URL (or absent, for root-relative links)"
        ));
    }
    if b.chars()
        .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '\\' | '`'))
    {
        return Err(format!(
            "base {base:?} contains a character a URL may not carry"
        ));
    }
    Ok(b.to_string())
}

/// The reader deep link for a node's address, mirroring
/// `web-code/src/lib/codeUrl.ts`'s `/r/<repo>/<path>?line=` grammar. A node
/// with no file address gets `None` — never a guessed link.
pub fn reader_link(base: &str, repo: &str, n: &NodeOut) -> Option<String> {
    let code = n.code.as_ref()?;
    let path: String = code
        .path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("/");
    if path.is_empty() {
        return None;
    }
    let repo_seg = percent_encode_segment(repo);
    let line = if code.range[0] == code.range[1] {
        format!("{}", code.range[0])
    } else {
        format!("{}-{}", code.range[0], code.range[1])
    };
    Some(format!("{base}/r/{repo_seg}/{path}?line={line}"))
}

/// `encodeURIComponent`'s unreserved set, so a link this crate builds and a
/// link the SPA builds for the same path are the same string.
fn percent_encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// --- Markdown ---------------------------------------------------------------

/// The Markdown export: one `##` per group (plus one for the ungrouped
/// remainder), nodes in STEP order where the board declares one, each node
/// as its address, its state, its note and its snippet.
pub fn to_markdown(board: &BoardOut) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", board.title));
    if !board.description_md.is_empty() {
        out.push_str(&board.description_md);
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "> `{}` · board `{}` · repo `{}` · status **{}** · revision {}\n>\n> {}\n\n",
        SCHEMA, board.slug, board.repo, board.status, board.revision, SNAPSHOT_CAPTION
    ));
    let h = &board.honesty;
    out.push_str(&format!(
        "**{} nodes** — {} pinned · {} carried · {} present · {} inert · **{} orphan**. \
         {} edges, {} steps.\n\n",
        h.nodes, h.pinned, h.carried, h.present, h.inert, h.orphans, h.edges, h.steps
    ));

    for (heading, ids) in sections(board) {
        out.push_str(&format!("## {heading}\n\n"));
        for id in ids {
            let Some(n) = board.nodes.iter().find(|n| n.id == id) else {
                continue;
            };
            out.push_str(&node_markdown(board, n));
        }
    }

    if !board.edges.is_empty() {
        out.push_str("## Edges\n\n");
        for e in &board.edges {
            let label = e.label.as_deref().unwrap_or("");
            let trust = match &e.trust {
                Some(t) => format!(" ({} {})", e.provenance, t),
                None => format!(" ({})", e.provenance),
            };
            out.push_str(&format!(
                "- `{}` —{}→ `{}`{}{}\n",
                e.from,
                e.kind,
                e.to,
                if label.is_empty() {
                    String::new()
                } else {
                    format!(" — {label}")
                },
                trust
            ));
        }
        out.push('\n');
    }
    if !h.notes.is_empty() {
        out.push_str("## Notes\n\n");
        for note in &h.notes {
            out.push_str(&format!("- {note}\n"));
        }
        out.push('\n');
    }
    out
}

fn node_markdown(board: &BoardOut, n: &NodeOut) -> String {
    let mut out = String::new();
    let step = board
        .steps
        .iter()
        .position(|s| s.node == n.id)
        .map(|i| format!("{}. ", i + 1))
        .unwrap_or_default();
    let title = n.title.clone().unwrap_or_else(|| n.id.clone());
    out.push_str(&format!("### {step}{title}\n\n"));
    out.push_str(&format!(
        "`{}` · **{}** ({}) · `{}`\n\n",
        n.kind, n.state, n.reason, n.address
    ));
    if let Some(caption) = board
        .steps
        .iter()
        .find(|s| s.node == n.id)
        .and_then(|s| s.caption.clone())
    {
        out.push_str(&format!("> {caption}\n\n"));
    }
    if let Some(b) = &n.body_md {
        out.push_str(b);
        out.push_str("\n\n");
    }
    if let Some(note) = &n.note {
        out.push_str(&format!("_{note}_\n\n"));
    }
    if let Some(c) = &n.code {
        if let Some(s) = &c.snippet {
            let lang = c
                .path
                .rsplit_once('.')
                .map(|(_, e)| e.to_string())
                .unwrap_or_default();
            out.push_str(&format!("```{lang}\n{s}\n```\n"));
            if c.snippet_truncated {
                out.push_str(&format!(
                    "\n_(snippet cut at {MAX_SNIPPET_LINES} lines; the range is \
                     {}-{})_\n",
                    c.range[0], c.range[1]
                ));
            }
            out.push('\n');
        } else if let Some(a) = &c.anchor_snippet {
            out.push_str(&format!(
                "_last known content (this code is gone):_\n\n```\n{a}\n```\n\n"
            ));
        }
    }
    if let Some(q) = &n.query {
        out.push_str(&format!("Query: `{}`\n\n", q.query));
        if let (Some(cur), Some(auth)) = (q.current_count, q.authored_count) {
            out.push_str(&format!(
                "{auth} → {cur} results ({:+}) since authored (count basis: {})\n\n",
                q.delta.unwrap_or(0),
                q.basis.unwrap_or("page")
            ));
        }
    }
    out
}

/// `(heading, node ids)` in reading order: one section per GROUP node (in
/// authored order), then everything ungrouped. Within a section, step order
/// first and authored order after — the same rule [`super::layout`] uses
/// for rows, so the linear and spatial projections agree.
fn sections(board: &BoardOut) -> Vec<(String, Vec<String>)> {
    let step_rank = |id: &str| {
        board
            .steps
            .iter()
            .position(|s| s.node == id)
            .unwrap_or(usize::MAX)
    };
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut placed: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for g in board.nodes.iter().filter(|n| n.kind == KIND_GROUP) {
        placed.insert(g.id.as_str());
        let mut ids: Vec<&NodeOut> = board
            .nodes
            .iter()
            .filter(|n| n.group.as_deref() == Some(g.id.as_str()))
            .collect();
        ids.sort_by_key(|n| {
            (
                step_rank(&n.id),
                board.nodes.iter().position(|x| x.id == n.id).unwrap_or(0),
            )
        });
        for n in &ids {
            placed.insert(n.id.as_str());
        }
        out.push((
            g.title.clone().unwrap_or_else(|| g.id.clone()),
            ids.into_iter().map(|n| n.id.clone()).collect(),
        ));
    }
    let mut rest: Vec<&NodeOut> = board
        .nodes
        .iter()
        .filter(|n| !placed.contains(n.id.as_str()))
        .collect();
    rest.sort_by_key(|n| {
        (
            step_rank(&n.id),
            board.nodes.iter().position(|x| x.id == n.id).unwrap_or(0),
        )
    });
    if !rest.is_empty() {
        out.push((
            if out.is_empty() {
                "Nodes".to_string()
            } else {
                "Ungrouped".to_string()
            },
            rest.into_iter().map(|n| n.id.clone()).collect(),
        ));
    }
    out
}

// --- JSON Canvas 1.0 --------------------------------------------------------

/// JSON Canvas 1.0. See the module doc for the lossiness contract.
pub fn to_json_canvas(board: &BoardOut) -> serde_json::Value {
    let nodes_in: Vec<(String, bool)> = board
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n.kind == KIND_GROUP))
        .collect();
    let edges_in: Vec<(String, String)> = board
        .edges
        .iter()
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    let steps_in: Vec<String> = board.steps.iter().map(|s| s.node.clone()).collect();
    let placed = layout::place(&nodes_in, &edges_in, &steps_in, &board.pins);

    let mut nodes = Vec::new();
    for n in &board.nodes {
        let p = placed.get(&n.id).copied().unwrap_or(layout::Placed {
            x: 0.0,
            y: 0.0,
            w: layout::CARD_W,
            h: layout::CARD_H,
        });
        let mut node = serde_json::json!({
            "id": n.id,
            // The spec's coordinates are integers.
            "x": p.x.round() as i64,
            "y": p.y.round() as i64,
            "width": p.w.round() as i64,
            "height": p.h.round() as i64,
            // Extension fields — namespaced, ignored by other apps per the
            // spec's own extensibility rule, and the ONLY place a range, a
            // trust class or a resolution state survives.
            "kbc_kind": n.kind,
            "kbc_state": n.state,
            "kbc_reason": n.reason,
            "kbc_address": n.address,
        });
        let obj = node.as_object_mut().expect("json! built an object");
        match n.kind.as_str() {
            KIND_GROUP => {
                obj.insert("type".into(), "group".into());
                obj.insert(
                    "label".into(),
                    n.title.clone().unwrap_or_else(|| n.id.clone()).into(),
                );
            }
            KIND_LINK => {
                obj.insert("type".into(), "link".into());
                obj.insert(
                    "url".into(),
                    n.reference.url.clone().unwrap_or_default().into(),
                );
            }
            KIND_CODE => {
                // A JSON Canvas `file` path is VAULT-relative and its
                // `subpath` is a Markdown heading anchor, so a line range
                // cannot ride it. The path goes in `file` (lossy but
                // useful in another app) and the range rides `kbc_range`.
                obj.insert("type".into(), "file".into());
                obj.insert(
                    "file".into(),
                    n.code
                        .as_ref()
                        .map(|c| c.path.clone())
                        .unwrap_or_default()
                        .into(),
                );
                if let Some(c) = &n.code {
                    obj.insert(
                        "kbc_range".into(),
                        serde_json::json!([c.range[0], c.range[1]]),
                    );
                    if let Some(b) = &c.current_blob_sha {
                        obj.insert("kbc_blob_sha".into(), b.clone().into());
                    }
                }
            }
            _ => {
                obj.insert("type".into(), "text".into());
                obj.insert("text".into(), text_body(n).into());
            }
        }
        nodes.push(node);
    }

    let edges: Vec<serde_json::Value> = board
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let mut edge = serde_json::json!({
                "id": format!("e{i}"),
                "fromNode": e.from,
                "toNode": e.to,
                "toEnd": "arrow",
                "kbc_kind": e.kind,
                "kbc_provenance": e.provenance,
            });
            let obj = edge.as_object_mut().expect("json! built an object");
            let label = match &e.label {
                Some(l) if !l.is_empty() => format!("{} — {l}", e.kind),
                _ => e.kind.clone(),
            };
            obj.insert("label".into(), label.into());
            if let Some(t) = &e.trust {
                obj.insert("kbc_trust".into(), t.clone().into());
            }
            edge
        })
        .collect();

    serde_json::json!({
        "nodes": nodes,
        "edges": edges,
        "kbc_schema": SCHEMA,
        "kbc_slug": board.slug,
        "kbc_repo": board.repo,
        "kbc_status": board.status,
        "kbc_revision": board.revision,
        "kbc_snapshot": SNAPSHOT_CAPTION,
        "kbc_layout": "derived by kb-code for THIS export only; the live board is \
                       coordinate-free plus pins and the SPA owns its layout",
    })
}

/// The plain-text body a `text`-typed JSON Canvas node carries — the same
/// assembly the Markdown export makes for one node, minus the headings.
fn text_body(n: &NodeOut) -> String {
    let mut out = String::new();
    if let Some(t) = &n.title {
        out.push_str(&format!("**{t}**\n\n"));
    }
    out.push_str(&format!("`{}` · {} · {}\n", n.kind, n.state, n.address));
    if let Some(b) = &n.body_md {
        out.push_str(&format!("\n{b}\n"));
    }
    if let Some(q) = &n.query {
        out.push_str(&format!("\nQuery: `{}`\n", q.query));
    }
    out
}

// --- kb-html ----------------------------------------------------------------

/// A single-file HTML artifact. See the module doc's three rules.
pub fn to_kb_html(board: &BoardOut, base: &str) -> String {
    let mut s = String::new();
    let title = format!("{} — kb-code board", board.title);
    s.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n");
    s.push_str("<meta charset=\"utf-8\">\n");
    s.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    s.push_str(&format!("<title>{}</title>\n", escape_text(&title)));
    s.push_str(&format!(
        "<meta name=\"kb-category\" content=\"{}\">\n",
        escape_attr(KB_CATEGORY)
    ));
    s.push_str(&format!(
        "<meta name=\"kb-tags\" content=\"{}\">\n",
        escape_attr(&kb_tags(board).join(", "))
    ));
    s.push_str(&format!(
        "<meta name=\"kb-summary\" content=\"{}\">\n",
        escape_attr(&format!(
            "{} — {} nodes, {} orphan, in {}",
            board.title, board.honesty.nodes, board.honesty.orphans, board.repo
        ))
    ));
    s.push_str(STYLE);
    s.push_str("</head>\n<body>\n");
    s.push_str(&format!("<h1>{}</h1>\n", escape_text(&board.title)));
    s.push_str(&format!(
        "<p class=\"meta\"><code>{}</code> · board <code>{}</code> · repo \
         <code>{}</code> · status <strong>{}</strong> · revision {}</p>\n",
        escape_text(SCHEMA),
        escape_text(&board.slug),
        escape_text(&board.repo),
        escape_text(&board.status),
        board.revision
    ));
    s.push_str(&format!(
        "<p class=\"caption\">{}</p>\n",
        escape_text(SNAPSHOT_CAPTION)
    ));
    if !board.description_md.is_empty() {
        // The board's description is authored MARKDOWN and is emitted as
        // escaped PRE text, not rendered. Rendering it would mean shipping
        // a Markdown renderer whose output this crate would then have to
        // sanitise — D9-a's "no LLM-authored HTML" ruling, applied to the
        // one surface that could have quietly broken it.
        s.push_str(&format!(
            "<pre class=\"md\">{}</pre>\n",
            escape_text(&board.description_md)
        ));
    }
    let h = &board.honesty;
    s.push_str("<h2 id=\"honesty\">Resolution</h2>\n<ul class=\"honesty\">\n");
    for (label, n) in [
        ("nodes", h.nodes),
        ("pinned", h.pinned),
        ("carried", h.carried),
        ("present", h.present),
        ("inert", h.inert),
        ("orphan", h.orphans),
        ("edges", h.edges),
        ("steps", h.steps),
        ("stale pins", h.stale_pins),
    ] {
        s.push_str(&format!(
            "<li>{}: <strong>{n}</strong></li>\n",
            escape_text(label)
        ));
    }
    s.push_str("</ul>\n");
    for note in &h.notes {
        s.push_str(&format!("<p class=\"note\">{}</p>\n", escape_text(note)));
    }

    for (heading, ids) in sections(board) {
        s.push_str(&format!(
            "<h2 id=\"{}\">{}</h2>\n",
            escape_attr(&section_id(&heading)),
            escape_text(&heading)
        ));
        for id in ids {
            let Some(n) = board.nodes.iter().find(|n| n.id == id) else {
                continue;
            };
            s.push_str(&node_html(board, n, base));
        }
    }

    if !board.edges.is_empty() {
        s.push_str("<h2 id=\"edges\">Edges</h2>\n<ul class=\"edges\">\n");
        for e in &board.edges {
            s.push_str(&format!(
                "<li><code>{}</code> —{}→ <code>{}</code>{} <span class=\"prov\">{}{}</span></li>\n",
                escape_text(&e.from),
                escape_text(&e.kind),
                escape_text(&e.to),
                match &e.label {
                    Some(l) if !l.is_empty() => format!(" — {}", escape_text(l)),
                    _ => String::new(),
                },
                escape_text(&e.provenance),
                match &e.trust {
                    Some(t) => format!(" · {}", escape_text(t)),
                    None => String::new(),
                }
            ));
        }
        s.push_str("</ul>\n");
    }
    s.push_str("</body>\n</html>\n");
    s
}

/// `kb-category` for every exported board. One fixed value: an export is
/// not a research artifact and must not land in a corpus claiming to be one.
pub const KB_CATEGORY: &str = "kb-code-board";

/// The board's `kb-tags`, DERIVED — a board carries no authored tag set, so
/// inventing one would be a field with no writer.
fn kb_tags(board: &BoardOut) -> Vec<String> {
    let mut tags = vec![
        "kb-code".to_string(),
        "board".to_string(),
        format!("repo:{}", board.repo),
        format!("status:{}", board.status),
    ];
    if board.honesty.orphans > 0 {
        tags.push("has-orphans".to_string());
    }
    tags
}

/// A stable, human-readable section id (root CLAUDE.md's authoring
/// contract: stable ids, no UUIDs, no timestamps).
fn section_id(heading: &str) -> String {
    let mut out = String::new();
    for c in heading.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    format!("s-{}", out.trim_matches('-'))
}

fn node_html(board: &BoardOut, n: &NodeOut, base: &str) -> String {
    let mut s = String::new();
    let step = board
        .steps
        .iter()
        .position(|x| x.node == n.id)
        .map(|i| format!("{}. ", i + 1))
        .unwrap_or_default();
    let title = n.title.clone().unwrap_or_else(|| n.id.clone());
    s.push_str(&format!(
        "<section class=\"card card--{}\" id=\"n-{}\">\n",
        escape_attr(n.state),
        escape_attr(&n.id)
    ));
    s.push_str(&format!(
        "<h3>{}{}</h3>\n",
        escape_text(&step),
        escape_text(&title)
    ));
    let address = match reader_link(base, &board.repo, n) {
        Some(href) => format!(
            "<a href=\"{}\"><code>{}</code></a>",
            escape_attr(&href),
            escape_text(&n.address)
        ),
        None => format!("<code>{}</code>", escape_text(&n.address)),
    };
    s.push_str(&format!(
        "<p class=\"addr\"><span class=\"kind\">{}</span> <span class=\"state\">{} \
         ({})</span> {}</p>\n",
        escape_text(&n.kind),
        escape_text(n.state),
        escape_text(n.reason),
        address
    ));
    if let Some(caption) = board
        .steps
        .iter()
        .find(|x| x.node == n.id)
        .and_then(|x| x.caption.as_deref())
    {
        s.push_str(&format!("<p class=\"step\">{}</p>\n", escape_text(caption)));
    }
    if let Some(b) = &n.body_md {
        s.push_str(&format!("<pre class=\"md\">{}</pre>\n", escape_text(b)));
    }
    if let Some(note) = &n.note {
        s.push_str(&format!("<p class=\"note\">{}</p>\n", escape_text(note)));
    }
    if let Some(c) = &n.code {
        if let Some(snippet) = &c.snippet {
            s.push_str(&format!(
                "<pre class=\"code\"><code>{}</code></pre>\n",
                escape_text(snippet)
            ));
            if c.snippet_truncated {
                s.push_str(&format!(
                    "<p class=\"note\">snippet cut at {MAX_SNIPPET_LINES} lines</p>\n"
                ));
            }
        } else if let Some(a) = &c.anchor_snippet {
            s.push_str(&format!(
                "<p class=\"note\">last known content (this code is gone):</p>\n\
                 <pre class=\"code code--gone\"><code>{}</code></pre>\n",
                escape_text(a)
            ));
        }
    }
    if let Some(q) = &n.query {
        s.push_str(&format!(
            "<p class=\"query\">query <code>{}</code>",
            escape_text(&q.query)
        ));
        if let (Some(cur), Some(auth)) = (q.current_count, q.authored_count) {
            s.push_str(&format!(
                " — {auth} → {cur} ({:+}, basis {})",
                q.delta.unwrap_or(0),
                escape_text(q.basis.unwrap_or("page"))
            ));
        }
        s.push_str("</p>\n");
    }
    s.push_str("</section>\n");
    s
}

/// The artifact's only stylesheet — written by this crate, containing no
/// interpolated value of any kind, which is what keeps rule 2 checkable by
/// reading it.
const STYLE: &str = r#"<style>
:root { color-scheme: light dark; }
body { font: 15px/1.6 ui-sans-serif, system-ui, sans-serif; max-width: 62rem;
       margin: 2rem auto; padding: 0 1rem; }
h1, h2, h3 { line-height: 1.25; }
code, pre { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; }
pre { overflow-x: auto; padding: .6rem .8rem; border-radius: 6px;
      background: rgba(127,127,127,.12); }
pre.md { white-space: pre-wrap; background: transparent; padding: 0; }
.card { border-left: 3px solid rgba(127,127,127,.4); padding-left: .9rem;
        margin: 1.4rem 0; }
.card--orphan { border-left-color: #b04b4b; opacity: .85; }
.card--carried { border-left-color: #b0904b; }
.card--pinned { border-left-color: #4b8fb0; }
.addr .kind, .addr .state { font-size: 12px; text-transform: uppercase;
                            letter-spacing: .04em; opacity: .75; }
.caption, .note, .meta { opacity: .75; font-size: 13px; }
.step { font-style: italic; }
.honesty { columns: 2; list-style: none; padding: 0; }
.edges { padding-left: 1.1rem; }
</style>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boards::resolve::{Budget, CodeCard, Honesty};

    fn honesty_of(nodes: usize, orphans: usize) -> Honesty {
        Honesty {
            nodes,
            edges: 0,
            steps: 0,
            pinned: nodes - orphans,
            carried: 0,
            orphans,
            present: 0,
            inert: 0,
            truncated_snippets: 0,
            stale_pins: 0,
            live_queries: false,
            budget: Budget {
                max_nodes: MAX_NODES,
                max_edges: MAX_EDGES,
                max_snippet_lines: MAX_SNIPPET_LINES,
            },
            notes: Vec::new(),
        }
    }

    fn node(id: &str, kind: &str) -> NodeOut {
        NodeOut {
            id: id.into(),
            kind: kind.into(),
            title: None,
            body_md: None,
            group: None,
            state: crate::boards::resolve::STATE_INERT,
            reason: crate::boards::resolve::REASON_NO_REFERENCE,
            address: id.into(),
            note: None,
            code: None,
            query: None,
            thread: None,
            pin: None,
            reference: RefFields::default(),
        }
    }

    fn board(nodes: Vec<NodeOut>) -> BoardOut {
        let n = nodes.len();
        BoardOut {
            schema: SCHEMA,
            repo: "monolith".into(),
            slug: "checkout-flow".into(),
            title: "Checkout".into(),
            description_md: String::new(),
            status: STATUS_ACCEPTED.into(),
            authored_ref: None,
            revision: 1,
            content_hash: "abc".into(),
            created_unix: 0,
            updated_unix: 0,
            nodes,
            edges: Vec::new(),
            steps: Vec::new(),
            pins: Default::default(),
            honesty: honesty_of(n, 0),
        }
    }

    #[test]
    fn escaping_is_amp_first_and_attributes_add_the_quote() {
        assert_eq!(escape_text("a & b < c > d"), "a &amp; b &lt; c &gt; d");
        assert_eq!(escape_text("<&>"), "&lt;&amp;&gt;");
        assert_eq!(
            escape_attr("\"x\" 'y' & <z>"),
            "&quot;x&quot; &#39;y&#39; &amp; &lt;z&gt;"
        );
        // The single-pass property: an `&` produced BY escaping must not be
        // escaped again.
        assert_eq!(escape_text("&lt;"), "&amp;lt;");
    }

    /// Every `<...>` region of an HTML document, so a test can assert on
    /// what is MARKUP rather than on substrings of escaped text. This
    /// distinction is the whole point: `onerror=` or `javascript:` sitting
    /// inside a `content="&lt;img …&gt;"` attribute VALUE is inert, and a
    /// bare substring check would fail a document that is perfectly safe.
    /// What matters is whether a payload ever became a tag, an attribute
    /// NAME, or a URL-bearing attribute's scheme.
    fn tag_regions(html: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = html;
        while let Some(i) = rest.find('<') {
            rest = &rest[i..];
            let Some(j) = rest.find('>') else { break };
            out.push(rest[..=j].to_string());
            rest = &rest[j + 1..];
        }
        out
    }

    /// The tag name of a region (`</p>` -> `p`, `<!doctype html>` ->
    /// `!doctype`), lowercased.
    fn tag_name(region: &str) -> String {
        region
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '!')
            .collect::<String>()
            .to_ascii_lowercase()
    }

    /// `(name, value)` for every attribute of a tag region, with
    /// `"`-quoted values scanned as ONE token.
    ///
    /// `escape_attr` escapes `"` (and `'`, `<`, `>`), so a payload can
    /// never terminate a value early — which is exactly why this scanner is
    /// allowed to be this simple, and why a payload that DID escape would
    /// show up here as a bogus attribute NAME rather than being swallowed.
    fn tag_attrs(region: &str) -> Vec<(String, String)> {
        let inner: String = region
            .trim_start_matches('<')
            .trim_end_matches('>')
            .trim_end_matches('/')
            .to_string();
        // Drop the tag name itself.
        let rest = match inner.find(char::is_whitespace) {
            Some(i) => inner[i..].to_string(),
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        let chars: Vec<char> = rest.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            let start = i;
            while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
                i += 1;
            }
            if i == start {
                break;
            }
            let name: String = chars[start..i].iter().collect();
            let mut value = String::new();
            if i < chars.len() && chars[i] == '=' {
                i += 1;
                if i < chars.len() && chars[i] == '"' {
                    i += 1;
                    let vs = i;
                    while i < chars.len() && chars[i] != '"' {
                        i += 1;
                    }
                    value = chars[vs..i].iter().collect();
                    i += 1; // closing quote
                } else {
                    let vs = i;
                    while i < chars.len() && !chars[i].is_whitespace() {
                        i += 1;
                    }
                    value = chars[vs..i].iter().collect();
                }
            }
            out.push((name.to_ascii_lowercase(), value));
        }
        out
    }

    /// The exact tag vocabulary `to_kb_html` emits. A payload that produced
    /// any other tag name got out of its text or attribute context.
    const EMITTED_TAGS: [&str; 19] = [
        "!doctype", "html", "head", "meta", "title", "style", "body", "h1", "h2", "h3", "p", "pre",
        "code", "ul", "li", "section", "a", "strong", "span",
    ];

    /// The attribute names `to_kb_html` writes. Anything else in an
    /// attribute-NAME position came from a payload.
    const EMITTED_ATTRS: [&str; 7] = ["lang", "charset", "name", "content", "class", "id", "href"];

    #[test]
    fn the_xss_corpus_never_survives_into_the_html() {
        let payloads = [
            "<script>alert(1)</script>",
            "\"><script>alert(1)</script>",
            "<img src=x onerror=alert(1)>",
            "</title><script>alert(1)</script>",
            "</pre><script>alert(1)</script>",
            "' onmouseover='alert(1)",
            "<!--<script>-->",
            "</section><h1>pwn",
            "</style><script>alert(1)</script>",
            "\" autofocus onfocus=\"alert(1)",
            "</a><a href=javascript:alert(1)>x",
        ];
        for p in payloads {
            let mut n = node("n1", KIND_NOTE);
            n.title = Some(p.to_string());
            n.body_md = Some(p.to_string());
            n.address = p.to_string();
            n.note = Some(p.to_string());
            let mut b = board(vec![n]);
            b.title = p.to_string();
            b.description_md = p.to_string();
            b.slug = p.to_string();
            b.repo = p.to_string();
            b.status = p.to_string();
            let html = to_kb_html(&b, "");

            for region in tag_regions(&html) {
                let tag = tag_name(&region);
                assert!(
                    EMITTED_TAGS.contains(&tag.as_str()),
                    "payload {p:?} produced the tag {region:?} — it escaped its context"
                );
                // `<!doctype html>`'s "attribute" is the doctype NAME, not
                // an attribute at all — the only region whose grammar is
                // not name/value pairs.
                if tag.starts_with('!') {
                    continue;
                }
                for (name, value) in tag_attrs(&region) {
                    assert!(
                        EMITTED_ATTRS.contains(&name.as_str()),
                        "payload {p:?} produced the attribute {name:?} in {region:?} — it \
                         escaped its value"
                    );
                    assert!(
                        !name.starts_with("on"),
                        "payload {p:?} produced an event handler {name:?} in {region:?}"
                    );
                    if name == "href" || name == "src" {
                        assert!(
                            value.starts_with('/')
                                || value.starts_with('#')
                                || value.starts_with("http://")
                                || value.starts_with("https://"),
                            "payload {p:?} produced the URL {value:?} in {region:?} — only a \
                             vetted scheme may reach an href"
                        );
                    }
                }
            }
            // Exactly one h1 and one title, whatever the payload did.
            assert_eq!(html.matches("<h1>").count(), 1, "payload {p:?}");
            assert_eq!(html.matches("<title>").count(), 1, "payload {p:?}");
            assert!(
                !html.to_ascii_lowercase().contains("<script"),
                "payload {p:?}"
            );
        }
    }

    #[test]
    fn the_artifact_carries_the_kb_metas_and_no_kb_prompt_template() {
        let b = board(vec![node("n1", KIND_NOTE)]);
        let html = to_kb_html(&b, "");
        assert!(html.contains("<meta name=\"kb-category\" content=\"kb-code-board\">"));
        assert!(html.contains("<meta name=\"kb-tags\""));
        assert!(html.contains("repo:monolith"));
        assert!(html.contains("status:accepted"));
        assert!(
            !html.contains("kb-prompt"),
            "the kb-prompt template is the kb side's to author, never this exporter's"
        );
        assert!(html.contains("<title>"));
        assert!(html.contains("<h1>"));
        assert!(html.contains("id=\"honesty\""));
        assert!(html.contains(SNAPSHOT_CAPTION));
    }

    #[test]
    fn a_link_nodes_url_never_becomes_an_href_scheme_this_export_did_not_vet() {
        // Lint refuses a non-http(s) url at apply time; this asserts the
        // export does not RE-introduce one as an href even if a row somehow
        // carried it (a hand-edited db, a future relaxation).
        let mut n = node("n1", KIND_LINK);
        n.reference.url = Some("javascript:alert(1)".into());
        n.address = "javascript:alert(1)".into();
        let b = board(vec![n]);
        let html = to_kb_html(&b, "");
        assert!(
            !html.contains("href=\"javascript:"),
            "a link node's url must never be emitted as an href by this exporter — \
             only a `code` node's reader link is"
        );
    }

    #[test]
    fn base_validation_refuses_everything_that_is_not_an_http_url() {
        assert_eq!(validate_base("").unwrap(), "");
        assert_eq!(
            validate_base("https://kbc.example/").unwrap(),
            "https://kbc.example"
        );
        for bad in [
            "javascript:alert(1)",
            "data:text/html,x",
            "//evil",
            "https://a b",
            "https://a\"b",
            "https://a'b",
            "https://a<b",
        ] {
            assert!(validate_base(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn the_reader_link_matches_the_spa_url_grammar() {
        let mut n = node("n1", KIND_CODE);
        n.code = Some(CodeCard {
            path: "app/models/order.rb".into(),
            symbol: None,
            range: [12, 48],
            context: None,
            authored_range: [12, 48],
            shifted_by: 0,
            authored_blob_sha: None,
            current_blob_sha: None,
            snippet: None,
            snippet_truncated: false,
            highlights: None,
            anchor_snippet: None,
            context_snippet: None,
        });
        assert_eq!(
            reader_link("", "monolith", &n).unwrap(),
            "/r/monolith/app/models/order.rb?line=12-48"
        );
        assert_eq!(
            reader_link("https://kbc.example", "mono lith", &n).unwrap(),
            "https://kbc.example/r/mono%20lith/app/models/order.rb?line=12-48"
        );
        // A node with no code card gets no link, never a guessed one.
        assert!(reader_link("", "r", &node("n2", KIND_NOTE)).is_none());
    }

    #[test]
    fn json_canvas_nodes_carry_every_required_spec_field() {
        let mut nodes = Vec::new();
        for k in NODE_KINDS {
            let mut n = node(&format!("n{k}"), k);
            if k == KIND_CODE {
                n.code = Some(CodeCard {
                    path: "a.rb".into(),
                    symbol: None,
                    range: [1, 2],
                    context: None,
                    authored_range: [1, 2],
                    shifted_by: 0,
                    authored_blob_sha: None,
                    current_blob_sha: Some("deadbeef".into()),
                    snippet: Some("x".into()),
                    snippet_truncated: false,
                    highlights: None,
                    anchor_snippet: None,
                    context_snippet: None,
                });
            }
            if k == KIND_LINK {
                n.reference.url = Some("https://example.test/x".into());
            }
            nodes.push(n);
        }
        let b = board(nodes);
        let v = to_json_canvas(&b);
        let arr = v["nodes"].as_array().unwrap();
        assert_eq!(arr.len(), NODE_KINDS.len());
        let mut ids = std::collections::BTreeSet::new();
        for n in arr {
            // JSON Canvas 1.0 required node fields.
            assert!(n["id"].is_string(), "{n}");
            assert!(
                ids.insert(n["id"].as_str().unwrap().to_string()),
                "duplicate id"
            );
            let ty = n["type"].as_str().expect("type");
            assert!(
                ["text", "file", "link", "group"].contains(&ty),
                "{ty} is not a JSON Canvas node type"
            );
            for f in ["x", "y", "width", "height"] {
                assert!(n[f].is_i64(), "{f} must be an integer: {n}");
            }
            match ty {
                "text" => assert!(n["text"].is_string()),
                "file" => assert!(n["file"].is_string()),
                "link" => assert!(n["url"].is_string()),
                "group" => assert!(n["label"].is_string()),
                _ => unreachable!(),
            }
            // The extension fields the spec cannot express.
            assert!(n["kbc_kind"].is_string());
            assert!(n["kbc_state"].is_string());
        }
        assert!(v["kbc_snapshot"].is_string());
        assert!(v["kbc_layout"].is_string());
    }

    #[test]
    fn json_canvas_edges_carry_the_required_fields_and_a_unique_id() {
        let mut b = board(vec![node("a", KIND_NOTE), node("b", KIND_NOTE)]);
        b.edges = vec![
            crate::boards::resolve::EdgeOut {
                from: "a".into(),
                to: "b".into(),
                kind: "calls".into(),
                label: Some("12 call sites".into()),
                provenance: PROVENANCE_DERIVED.into(),
                trust: Some("likely".into()),
            },
            crate::boards::resolve::EdgeOut {
                from: "b".into(),
                to: "a".into(),
                kind: "then".into(),
                label: None,
                provenance: PROVENANCE_AUTHORED.into(),
                trust: None,
            },
        ];
        let v = to_json_canvas(&b);
        let edges = v["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 2);
        assert_ne!(edges[0]["id"], edges[1]["id"]);
        for e in edges {
            assert!(e["id"].is_string());
            assert!(e["fromNode"].is_string());
            assert!(e["toNode"].is_string());
            assert!(e["label"].is_string());
        }
        assert_eq!(edges[0]["kbc_trust"], "likely");
        assert!(edges[1].get("kbc_trust").is_none());
    }

    #[test]
    fn the_markdown_export_is_a_stable_golden() {
        let mut g = node("g-domain", KIND_GROUP);
        g.title = Some("Domain".into());
        let mut n1 = node("n-place", KIND_CODE);
        n1.title = Some("Entry point".into());
        n1.group = Some("g-domain".into());
        n1.state = crate::boards::resolve::STATE_PINNED;
        n1.reason = crate::boards::resolve::REASON_BLOB_CURRENT;
        n1.address = "app/models/order.rb:112-114".into();
        n1.body_md = Some("Everything downstream hangs off this.".into());
        n1.code = Some(CodeCard {
            path: "app/models/order.rb".into(),
            symbol: Some("Order#place".into()),
            range: [112, 114],
            context: None,
            authored_range: [112, 114],
            shifted_by: 0,
            authored_blob_sha: None,
            current_blob_sha: None,
            snippet: Some("def place\n  lock!\nend".into()),
            snippet_truncated: false,
            highlights: None,
            anchor_snippet: None,
            context_snippet: None,
        });
        let mut n2 = node("n-gone", KIND_CODE);
        n2.state = crate::boards::resolve::STATE_ORPHAN;
        n2.reason = crate::boards::resolve::REASON_PATH_GONE;
        n2.address = "app/old.rb:1-2".into();
        n2.code = Some(CodeCard {
            path: "app/old.rb".into(),
            symbol: None,
            range: [1, 2],
            context: None,
            authored_range: [1, 2],
            shifted_by: 0,
            authored_blob_sha: None,
            current_blob_sha: None,
            snippet: None,
            snippet_truncated: false,
            highlights: None,
            anchor_snippet: Some("def legacy".into()),
            context_snippet: None,
        });
        let mut b = board(vec![g, n1, n2]);
        b.honesty = honesty_of(3, 1);
        b.steps = vec![crate::boards::resolve::StepOut {
            node: "n-place".into(),
            caption: Some("Start here.".into()),
        }];
        b.honesty.steps = 1;
        let md = to_markdown(&b);
        insta_like(&md);
    }

    /// The Markdown golden, inline (this crate has no snapshot harness) —
    /// asserted as a set of REQUIRED lines rather than one byte blob, so an
    /// unrelated wording change to one section does not fail the whole
    /// export.
    fn insta_like(md: &str) {
        for required in [
            "# Checkout\n",
            "## Domain\n",
            "### 1. Entry point\n",
            "`code` · **pinned** (blob-current) · `app/models/order.rb:112-114`",
            "> Start here.",
            "Everything downstream hangs off this.",
            "```rb\ndef place\n  lock!\nend\n```",
            "## Ungrouped\n",
            "`code` · **orphan** (path-gone) · `app/old.rb:1-2`",
            "_last known content (this code is gone):_",
            "**3 nodes** — 2 pinned · 0 carried · 0 present · 0 inert · **1 orphan**",
        ] {
            assert!(
                md.contains(required),
                "the Markdown export is missing {required:?}\n---\n{md}"
            );
        }
    }

    #[test]
    fn section_ids_are_stable_and_slug_shaped() {
        assert_eq!(section_id("Domain"), "s-domain");
        assert_eq!(section_id("The Checkout Flow!"), "s-the-checkout-flow");
        assert_eq!(section_id("  "), "s-");
    }

    #[test]
    fn every_declared_format_is_valid_and_has_a_content_type_and_filename() {
        for f in FORMATS {
            assert!(is_valid_format(f));
            assert!(!content_type(f).is_empty());
            assert!(filename("b", f).starts_with("b."));
        }
        assert!(!is_valid_format("svg"));
        assert_eq!(filename("b", FORMAT_JSONCANVAS), "b.canvas");
    }
}
