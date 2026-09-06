//! `GET /api/kb/{kb}/graph/{id}[?depth=N]` — neighborhood graph for a
//! single artifact. v0.2 returned the heading hierarchy (h1..h6) as
//! nodes plus parent->child "contains" edges. v0.3 adds:
//!
//!   1. cross-artifact `link` edges read from the sqlite `edges` table
//!      (populated by the indexer's F1 link-graph extraction step), and
//!   2. a `?depth=N` query param (clamped to 1..=3) controlling how
//!      far the BFS walks.
//!
//! The TUI DETAIL tab consumes this for an ASCII tree (V3 layout per
//! topic 05). The SPA may use it for a sidebar outline or hover popover.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::Response,
    response::IntoResponse,
    Json,
};
use kb_core::parser::headings_in_order;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Hard cap on `?depth=N` (matches `Db::edges_from` clamp). Keeps the
/// BFS bounded so a malicious or accidental depth=99 doesn't fan out.
const MAX_DEPTH: u32 = 3;

#[derive(Debug, Serialize)]
pub struct GraphResponse {
    pub artifact_id: String,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Serialize)]
pub struct Node {
    pub id: String,
    pub title: String,
    /// Heading level for "heading" nodes, `0` for cross-artifact "link" nodes.
    pub level: u8,
    /// `"heading"` for the local outline tree, `"artifact"` for nodes
    /// added via the cross-artifact graph (different consumer rendering).
    pub kind: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: &'static str,
    /// BFS depth for cross-artifact edges (1..=MAX_DEPTH); `0` for the
    /// heading hierarchy edges (those are tree-shaped, not BFS).
    pub depth: u32,
}

#[derive(Debug, Deserialize, Default)]
pub struct GraphQuery {
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ReportQuery {
    /// Hubs-list cap; default 10, clamped to 1..=100.
    pub top: Option<usize>,
}

/// GS-track — `GET /api/kb/{kb}/graph/report[?top=N]`. The deterministic
/// corpus graph report (`kb_core::graph_report::build`): hubs, orphans
/// (never linked ∧ never opened), dead-edge link-rot, and dangling/
/// ambiguous wikilinks re-resolved over Markdown sources. Pure read —
/// never bumps the index generation. The Markdown re-parse reads source
/// bytes from disk (same files the indexer walks), so cost scales with
/// the corpus's Markdown footprint; an explicit operator verb, not a
/// hot-path dependency.
pub async fn report(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Path(kb): Path<String>,
    Query(params): Query<ReportQuery>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let top = params.top.unwrap_or(10).clamp(1, 100);

    // PF-R1 — reuse the per-(kb, generation) gallery row-set memo
    // (invariant #15) instead of an independent `list_docs(u32::MAX)` actor
    // round-trip; this route is an explicit operator verb, not a hot path
    // (see module doc), but still benefits when the memo is already warm
    // from `/docs`/`/facets`/`/edges`, and costs nothing extra when cold.
    // NB: the memo's OWN `edge_counts` half is deliberately NOT reused for
    // `degrees` below — `gallery_snapshot` folds an `edge_counts()` failure
    // to an empty map (`unwrap_or_default`, the established contract every
    // other memo consumer accepts), whereas this route has always hard-
    // errored on that failure; keeping the separate call preserves that
    // existing error contract byte-for-byte.
    let (rows, _edge_counts) = match crate::routes::docs::gallery_snapshot(ctx).await {
        Ok(v) => v,
        Err(e) => return error_to_problem_json(&e),
    };
    let edges = match ctx.storage.link_pairs().await {
        Ok(e) => e,
        Err(e) => return error_to_problem_json(&e),
    };
    let degrees = match ctx.storage.edge_counts().await {
        Ok(d) => d,
        Err(e) => return error_to_problem_json(&e),
    };
    // Strict "ever opened": a real `kind='open'` visit. Entries introduced
    // by a list read_override alone carry `last_opened_unix: None` and must
    // not count (invariant #19 — asserted ≠ opened).
    // v0.34 Y1 — requester's user.
    let opened: std::collections::HashSet<String> =
        match ctx.storage.reading_rollup(identity.user.clone()).await {
            Ok(r) => r
                .into_iter()
                .filter(|(_, v)| v.last_opened_unix.is_some())
                .map(|(k, _)| k)
                .collect(),
            Err(e) => return error_to_problem_json(&e),
        };

    let mut docs: Vec<kb_core::graph_report::ReportDoc> = Vec::with_capacity(rows.len());
    for r in rows.iter() {
        let rel = kb_core::paths::doc_rel_path(&r.doc.path, &ctx.source_path);
        // SC5 — `ctx.ext_map`, not the hardcoded extension check, so a mapped
        // extension's wikilinks are scanned here too (agreeing with the
        // index-time edge set once the source lands on the same resolved map).
        let md = if ctx.ext_map.is_markdown(std::path::Path::new(&r.doc.path)) {
            // Best-effort: a source that vanished mid-report just skips the
            // wikilink pass (the doc row itself still counts).
            tokio::fs::read_to_string(&r.doc.path).await.ok()
        } else {
            None
        };
        docs.push(kb_core::graph_report::ReportDoc {
            id: r.doc.id.clone(),
            rel_path: rel,
            title: r.doc.title.clone(),
            markdown_source: md,
        });
    }
    // Deterministic input order regardless of lance scan order.
    docs.sort_by(|a, b| a.id.cmp(&b.id));

    let report = kb_core::graph_report::build(&docs, &edges, &degrees, &opened, top);
    Json(report).into_response()
}

pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Query(params): Query<GraphQuery>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };

    let row = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(r)) => r,
        Ok(None) => return error_to_problem_json(&kb_core::Error::NotFound(format!("doc {id}"))),
        Err(e) => return error_to_problem_json(&e),
    };

    let bytes = match std::fs::read(&row.path) {
        Ok(b) => b,
        Err(e) => return error_to_problem_json(&kb_core::Error::Io(e)),
    };
    let html = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => {
            return error_to_problem_json(&kb_core::Error::BadRequest("artifact not utf-8".into()))
        }
    };

    let (mut nodes, mut edges) = build_heading_graph(html);

    let depth = params.depth.unwrap_or(1).clamp(1, MAX_DEPTH);
    match ctx.storage.edges_from(id.clone(), depth).await {
        Ok(rows) => {
            let mut seen_targets = std::collections::HashSet::new();
            for e in rows {
                if seen_targets.insert(e.to_id.clone()) {
                    nodes.push(Node {
                        id: e.to_id.clone(),
                        title: e.to_id.clone(),
                        level: 0,
                        kind: "artifact",
                    });
                }
                edges.push(Edge {
                    from: e.from_id,
                    to: e.to_id,
                    kind: "link",
                    depth: e.depth,
                });
            }
        }
        Err(e) => {
            // The cross-artifact graph is enrichment, not a contract —
            // log + drop it rather than failing the route.
            tracing::warn!(kb = %kb_name, id = %id, error = %e, "edges_from failed");
        }
    }

    Json(GraphResponse {
        artifact_id: id,
        nodes,
        edges,
    })
    .into_response()
}

/// Stack-walk the flat (level, title) list returned by kb-core into a
/// tree. Each heading at level N attaches to the most recent heading at
/// level <N. Slugs are deduplicated with `-2`, `-3`, ... suffixes.
fn build_heading_graph(html: &str) -> (Vec<Node>, Vec<Edge>) {
    let flat = headings_in_order(html);
    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut stack: Vec<(u8, String)> = Vec::new();
    let mut slug_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();

    for (level, title) in flat {
        let base = slugify(&title);
        let count = slug_counts.entry(base.clone()).or_insert(0);
        *count += 1;
        let id = if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        };
        while let Some(&(top_level, _)) = stack.last() {
            if top_level >= level {
                stack.pop();
            } else {
                break;
            }
        }
        if let Some((_, parent)) = stack.last() {
            edges.push(Edge {
                from: parent.clone(),
                to: id.clone(),
                kind: "contains",
                depth: 0,
            });
        }
        stack.push((level, id.clone()));
        nodes.push(Node {
            id,
            title,
            level,
            kind: "heading",
        });
    }
    (nodes, edges)
}

/// Lowercased ascii-alphanumeric slug; spaces/punct collapse to `-`.
/// Empty → "section".
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "section".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_h2_list_yields_no_edges_above_h1() {
        let html = "<h1>Title</h1><h2>One</h2><h2>Two</h2><h2>Three</h2>";
        let (nodes, edges) = build_heading_graph(html);
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[0].level, 1);
        assert!(nodes[1..].iter().all(|n| n.level == 2));
        assert_eq!(edges.len(), 3);
        assert!(edges.iter().all(|e| e.from == "title"));
        assert!(edges.iter().all(|e| e.kind == "contains"));
        assert!(nodes.iter().all(|n| n.kind == "heading"));
    }

    #[test]
    fn nested_headings_build_proper_tree() {
        let html = r#"
            <h1>Doc</h1>
              <h2>Intro</h2>
                <h3>Why</h3>
                <h3>How</h3>
              <h2>Detail</h2>
                <h3>Schema</h3>
        "#;
        let (nodes, edges) = build_heading_graph(html);
        assert_eq!(nodes.len(), 6);
        assert_eq!(edges.len(), 5);
        assert!(edges.iter().any(|e| e.from == "doc" && e.to == "intro"));
        assert!(edges.iter().any(|e| e.from == "intro" && e.to == "why"));
        assert!(edges.iter().any(|e| e.from == "detail" && e.to == "schema"));
    }

    #[test]
    fn duplicate_titles_disambiguate_with_suffix() {
        let html = "<h2>Setup</h2><h2>Setup</h2><h2>Setup</h2>";
        let (nodes, _) = build_heading_graph(html);
        assert_eq!(nodes[0].id, "setup");
        assert_eq!(nodes[1].id, "setup-2");
        assert_eq!(nodes[2].id, "setup-3");
    }

    #[test]
    fn empty_document_yields_empty_graph() {
        let (nodes, edges) = build_heading_graph("<p>no headings here</p>");
        assert!(nodes.is_empty());
        assert!(edges.is_empty());
    }

    #[test]
    fn h3_under_h1_skips_to_h1() {
        let html = "<h1>Top</h1><h3>Mid</h3><h2>After</h2>";
        let (_, edges) = build_heading_graph(html);
        assert!(edges.iter().any(|e| e.from == "top" && e.to == "mid"));
        assert!(edges.iter().any(|e| e.from == "top" && e.to == "after"));
    }

    #[test]
    fn slugify_handles_unicode_and_punctuation() {
        assert_eq!(slugify("Hello, World!"), "hello-world");
        assert_eq!(slugify("  spaced  out  "), "spaced-out");
        assert_eq!(slugify("foo--bar---baz"), "foo-bar-baz");
        assert_eq!(slugify("!!!"), "section");
        assert_eq!(slugify("Émoji✨here"), "moji-here");
    }
}
