//! GS-track — the deterministic corpus graph report.
//!
//! A pure function over data the daemon already holds: the `edges` table
//! (`link_pairs` + `edge_counts`), the live doc set, the reading rollup's
//! "ever opened" signal, and — for Markdown sources — a fresh wikilink
//! re-resolution via [`crate::links`]. It answers four questions the graph
//! quietly accumulates but nothing surfaces:
//!
//! - **hubs** — the most-linked artifacts (in-degree first: being linked
//!   *to* is the endorsement axis; fan-out is cheap).
//! - **orphans** — never linked (absent from `edges` in both directions)
//!   AND never opened (no `kind='open'` history visit). "Touched by a
//!   session" is deliberately NOT folded in (invariant #19: touched ≠ read).
//! - **link-rot** — edges whose `dst` (or `src`) no longer names a live
//!   artifact. Dead-`dst` rows are expected between a target's deletion and
//!   the source's next reindex (`sweep_orphans` prunes dead-`src` only);
//!   they also fingerprint pre-2026-07-03 path-form parser rows that a
//!   `kb reindex` clears.
//! - **dangling / ambiguous wikilinks** — `[[targets]]` in Markdown sources
//!   that resolve to nothing (or to several docs). The edge hook discards
//!   these at index time (`Resolution::One`-only); the report re-parses and
//!   keeps them.
//!
//! Determinism discipline (same spirit as the atlas, kb-core invariant #3):
//! every output list is explicitly sorted — nothing inherits HashMap
//! iteration order — and no clock is read; the report is a pure function of
//! its inputs.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::links::{self, DocLite, Resolution};

/// One artifact's input projection. `markdown_source` is `Some(raw)` only
/// for Markdown docs — the wikilink re-resolution pass runs on those (the
/// same `is_markdown` gate the edge hook uses; wikilinks don't exist in
/// HTML sources).
pub struct ReportDoc {
    pub id: String,
    /// Source-relative, forward-slash path (the resolution ladder's tier 2).
    pub rel_path: String,
    pub title: String,
    pub markdown_source: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct HubRow {
    pub id: String,
    pub title: String,
    pub rel_path: String,
    pub inbound: u32,
    pub outbound: u32,
}

#[derive(Debug, Serialize)]
pub struct OrphanRow {
    pub id: String,
    pub title: String,
    pub rel_path: String,
}

/// An edge row that references a non-live artifact id on one side.
#[derive(Debug, Serialize)]
pub struct RotEdge {
    pub src: String,
    pub dst: String,
    /// The live side's rel_path, when the live side exists — makes the row
    /// actionable ("reindex/fix THIS file") without a second lookup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_rel_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UnresolvedLink {
    pub src_id: String,
    pub src_rel_path: String,
    /// The `[[target]]` as written (normalized).
    pub target: String,
    /// `"dangling"` (no match) or `"ambiguous"` (several matches).
    pub state: &'static str,
    /// Candidate ids for the ambiguous case; empty when dangling.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub candidates: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GraphReport {
    // Totals first — the one-screen summary.
    pub docs: usize,
    pub edges: usize,
    pub linked_docs: usize,
    pub never_linked: usize,
    pub opened_docs: usize,
    pub orphan_count: usize,
    pub max_in_degree: u32,
    pub max_out_degree: u32,
    // Detail sections, every list deterministically sorted.
    pub hubs: Vec<HubRow>,
    pub orphans: Vec<OrphanRow>,
    pub dead_dst_edges: Vec<RotEdge>,
    pub dead_src_edges: Vec<RotEdge>,
    pub unresolved_wikilinks: Vec<UnresolvedLink>,
}

/// Build the report. `edges` are `(src, dst)` pairs (`link_pairs`),
/// `degrees` is the `edge_counts` map (`id → (outbound, inbound)`),
/// `opened` is the set of artifact ids with at least one open visit,
/// `top` caps the hubs list.
pub fn build(
    docs: &[ReportDoc],
    edges: &[(String, String)],
    degrees: &HashMap<String, (u32, u32)>,
    opened: &HashSet<String>,
    top: usize,
) -> GraphReport {
    let live: HashMap<&str, &ReportDoc> = docs.iter().map(|d| (d.id.as_str(), d)).collect();

    // Link-rot: an edge row referencing a non-live id on either side.
    let mut dead_dst: Vec<RotEdge> = Vec::new();
    let mut dead_src: Vec<RotEdge> = Vec::new();
    for (src, dst) in edges {
        if !live.contains_key(dst.as_str()) {
            dead_dst.push(RotEdge {
                src: src.clone(),
                dst: dst.clone(),
                src_rel_path: live.get(src.as_str()).map(|d| d.rel_path.clone()),
            });
        }
        if !live.contains_key(src.as_str()) {
            dead_src.push(RotEdge {
                src: src.clone(),
                dst: dst.clone(),
                src_rel_path: None,
            });
        }
    }
    dead_dst.sort_by(|a, b| (&a.src, &a.dst).cmp(&(&b.src, &b.dst)));
    dead_src.sort_by(|a, b| (&a.src, &a.dst).cmp(&(&b.src, &b.dst)));

    // Degree-derived sets. `degrees` covers every id that appears in the
    // edges table; a live doc absent from it has degree (0,0).
    let linked_live: usize = docs
        .iter()
        .filter(|d| degrees.get(&d.id).is_some_and(|&(o, i)| o > 0 || i > 0))
        .count();
    let mut never_linked: Vec<&ReportDoc> = docs
        .iter()
        .filter(|d| degrees.get(&d.id).is_none_or(|&(o, i)| o == 0 && i == 0))
        .collect();
    never_linked.sort_by(|a, b| a.id.cmp(&b.id));

    let mut orphans: Vec<OrphanRow> = never_linked
        .iter()
        .filter(|d| !opened.contains(&d.id))
        .map(|d| OrphanRow {
            id: d.id.clone(),
            title: d.title.clone(),
            rel_path: d.rel_path.clone(),
        })
        .collect();
    orphans.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // Hubs: in-degree desc, then out-degree desc, then id — a total order.
    let mut hub_ids: Vec<&ReportDoc> = docs
        .iter()
        .filter(|d| degrees.get(&d.id).is_some_and(|&(o, i)| o > 0 || i > 0))
        .collect();
    hub_ids.sort_by(|a, b| {
        let da = degrees.get(&a.id).copied().unwrap_or((0, 0));
        let db = degrees.get(&b.id).copied().unwrap_or((0, 0));
        (db.1, db.0, &a.id).cmp(&(da.1, da.0, &b.id))
    });
    let hubs: Vec<HubRow> = hub_ids
        .into_iter()
        .take(top)
        .map(|d| {
            let (o, i) = degrees.get(&d.id).copied().unwrap_or((0, 0));
            HubRow {
                id: d.id.clone(),
                title: d.title.clone(),
                rel_path: d.rel_path.clone(),
                inbound: i,
                outbound: o,
            }
        })
        .collect();

    // Wikilink re-resolution over Markdown sources — the exact ladder the
    // edge hook runs, but keeping the outcomes it discards.
    let candidates: Vec<DocLite> = docs
        .iter()
        .map(|d| DocLite {
            id: d.id.clone(),
            rel_path: d.rel_path.clone(),
            title: d.title.clone(),
        })
        .collect();
    let mut unresolved: Vec<UnresolvedLink> = Vec::new();
    for d in docs {
        let Some(src) = d.markdown_source.as_deref() else {
            continue;
        };
        if !src.contains("[[") {
            continue;
        }
        let mut seen: HashSet<String> = HashSet::new();
        for wl in links::parse_wikilinks(src) {
            let norm = links::normalize_target(&wl.target);
            if norm.is_empty() || !seen.insert(norm.clone()) {
                continue;
            }
            match links::resolve(&wl.target, &candidates) {
                Resolution::One(_) => {}
                Resolution::None => unresolved.push(UnresolvedLink {
                    src_id: d.id.clone(),
                    src_rel_path: d.rel_path.clone(),
                    target: norm,
                    state: "dangling",
                    candidates: Vec::new(),
                }),
                Resolution::Ambiguous(mut ids) => {
                    ids.sort();
                    unresolved.push(UnresolvedLink {
                        src_id: d.id.clone(),
                        src_rel_path: d.rel_path.clone(),
                        target: norm,
                        state: "ambiguous",
                        candidates: ids,
                    })
                }
            }
        }
    }
    unresolved.sort_by(|a, b| (&a.src_rel_path, &a.target).cmp(&(&b.src_rel_path, &b.target)));

    let max_in = degrees.values().map(|&(_, i)| i).max().unwrap_or(0);
    let max_out = degrees.values().map(|&(o, _)| o).max().unwrap_or(0);

    GraphReport {
        docs: docs.len(),
        edges: edges.len(),
        linked_docs: linked_live,
        never_linked: never_linked.len(),
        opened_docs: docs.iter().filter(|d| opened.contains(&d.id)).count(),
        orphan_count: orphans.len(),
        max_in_degree: max_in,
        max_out_degree: max_out,
        hubs,
        orphans,
        dead_dst_edges: dead_dst,
        dead_src_edges: dead_src,
        unresolved_wikilinks: unresolved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(id: &str, rel: &str, title: &str, md: Option<&str>) -> ReportDoc {
        ReportDoc {
            id: id.into(),
            rel_path: rel.into(),
            title: title.into(),
            markdown_source: md.map(str::to_string),
        }
    }

    fn deg(pairs: &[(&str, u32, u32)]) -> HashMap<String, (u32, u32)> {
        pairs
            .iter()
            .map(|(id, o, i)| (id.to_string(), (*o, *i)))
            .collect()
    }

    #[test]
    fn orphan_requires_never_linked_and_never_opened() {
        let docs = vec![
            rd("aaaaaaaaaaa1", "a.html", "A", None), // linked
            rd("aaaaaaaaaaa2", "b.html", "B", None), // unlinked, opened
            rd("aaaaaaaaaaa3", "c.html", "C", None), // unlinked, unopened → orphan
        ];
        let edges = vec![("aaaaaaaaaaa1".to_string(), "aaaaaaaaaaa2".to_string())];
        // NB: degrees deliberately marks only a1 (as if b's inbound row were
        // missing) to exercise the is_none_or path… but keep it consistent:
        let degrees = deg(&[("aaaaaaaaaaa1", 1, 0), ("aaaaaaaaaaa2", 0, 1)]);
        let opened: HashSet<String> = ["aaaaaaaaaaa2".to_string()].into();
        let r = build(&docs, &edges, &degrees, &opened, 10);
        assert_eq!(r.orphan_count, 1);
        assert_eq!(r.orphans[0].id, "aaaaaaaaaaa3");
        assert_eq!(r.never_linked, 1, "b is linked (inbound), c is not");
        assert_eq!(r.linked_docs, 2);
    }

    #[test]
    fn dead_dst_edge_detected_and_src_rel_path_attached() {
        let docs = vec![rd("aaaaaaaaaaa1", "src.html", "S", None)];
        let edges = vec![("aaaaaaaaaaa1".to_string(), "deadbeef0000".to_string())];
        let degrees = deg(&[("aaaaaaaaaaa1", 1, 0), ("deadbeef0000", 0, 1)]);
        let r = build(&docs, &edges, &degrees, &HashSet::new(), 10);
        assert_eq!(r.dead_dst_edges.len(), 1);
        assert_eq!(r.dead_dst_edges[0].dst, "deadbeef0000");
        assert_eq!(
            r.dead_dst_edges[0].src_rel_path.as_deref(),
            Some("src.html")
        );
        assert!(r.dead_src_edges.is_empty());
    }

    #[test]
    fn hubs_rank_by_inbound_then_outbound_then_id() {
        let docs = vec![
            rd("aaaaaaaaaaa1", "1.html", "One", None),
            rd("aaaaaaaaaaa2", "2.html", "Two", None),
            rd("aaaaaaaaaaa3", "3.html", "Three", None),
        ];
        let degrees = deg(&[
            ("aaaaaaaaaaa1", 9, 2),
            ("aaaaaaaaaaa2", 0, 5),
            ("aaaaaaaaaaa3", 1, 5),
        ]);
        let r = build(&docs, &[], &degrees, &HashSet::new(), 2);
        // in=5 beats in=2 regardless of out; among in=5, out=1 beats out=0.
        assert_eq!(r.hubs.len(), 2);
        assert_eq!(r.hubs[0].id, "aaaaaaaaaaa3");
        assert_eq!(r.hubs[1].id, "aaaaaaaaaaa2");
    }

    #[test]
    fn wikilink_reresolution_reports_dangling_and_ambiguous() {
        let docs = vec![
            rd(
                "aaaaaaaaaaa1",
                "notes/n.md",
                "N",
                Some("see [[missing page]] and [[deploy]] twice: [[deploy]]"),
            ),
            rd("aaaaaaaaaaa2", "ops/deploy.md", "Ops deploy", None),
            rd("aaaaaaaaaaa3", "infra/deploy.md", "Infra deploy", None),
        ];
        let r = build(&docs, &[], &HashMap::new(), &HashSet::new(), 10);
        assert_eq!(
            r.unresolved_wikilinks.len(),
            2,
            "duplicates dedup per target"
        );
        let dangle = &r.unresolved_wikilinks[1];
        assert_eq!(
            (dangle.target.as_str(), dangle.state),
            ("missing page", "dangling")
        );
        let ambig = &r.unresolved_wikilinks[0];
        assert_eq!(
            (ambig.target.as_str(), ambig.state),
            ("deploy", "ambiguous")
        );
        assert_eq!(ambig.candidates, vec!["aaaaaaaaaaa2", "aaaaaaaaaaa3"]);
    }

    #[test]
    fn output_is_deterministic_regardless_of_input_order() {
        let mk = |flip: bool| {
            let mut docs = vec![
                rd("aaaaaaaaaaa1", "a.html", "A", None),
                rd("aaaaaaaaaaa2", "b.html", "B", None),
            ];
            let mut edges = vec![
                ("aaaaaaaaaaa1".to_string(), "dead00000001".to_string()),
                ("aaaaaaaaaaa2".to_string(), "dead00000000".to_string()),
            ];
            if flip {
                docs.reverse();
                edges.reverse();
            }
            let degrees = deg(&[("aaaaaaaaaaa1", 1, 0), ("aaaaaaaaaaa2", 1, 0)]);
            let r = build(&docs, &edges, &degrees, &HashSet::new(), 10);
            serde_json::to_string(&r).unwrap()
        };
        assert_eq!(mk(false), mk(true));
    }
}
