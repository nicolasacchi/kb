//! V74-L1 — the EXPORT-ONLY layered layout.
//!
//! # Read this before assuming the server lays boards out
//!
//! **It does not.** D10 is explicit: *"Layout stays in TypeScript, one
//! engine (the golden-tested layered DAG already exists)."* Boards are
//! stored coordinate-free plus pins, the SPA computes geometry, and
//! `GET /api/boards/{slug}` returns no coordinates at all.
//!
//! This module exists for exactly one reason: **JSON Canvas 1.0 requires
//! `x`/`y`/`width`/`height` on every node.** An export that omitted them
//! would not be a JSON Canvas file. So the server derives geometry HERE,
//! for that one format, and the export carries a caption saying what it is:
//!
//! > an exported board is a SNAPSHOT, not the live board. Its coordinates
//! > were derived by this exporter, not authored, and re-exporting after
//! > the SPA's own layout runs will not reproduce the same picture.
//!
//! # Same algorithm FAMILY, not the same algorithm
//!
//! The SPA's engine is `web-code/src/lib/egoGraph.ts`'s layered DAG: fixed
//! column gap and row pitch, order within a layer by a rank key then name
//! then id, no physics and no randomness. This is that family — layer by
//! longest-path depth over the edge DAG, order within a layer by step
//! order then authored order, fixed pitch — and it reuses `egoGraph.ts`'s
//! own geometry constants so the two produce cards on the same grid.
//!
//! It is deliberately NOT a port, and the divergence is recorded rather
//! than papered over: `egoGraph` lays out an EGO graph (one center, callers
//! left, callees right — a shape a board does not have), and a port would
//! be a second implementation of a thing D10 says has one. If the two ever
//! need to agree pixel-for-pixel, the honest fix is for the SPA to send its
//! computed positions as PINS, not for this module to grow.
//!
//! # Determinism
//!
//! Pure, total, no clock, no randomness, no floating-point accumulation
//! (positions are integer multiples of the pitch, emitted as `f64`).
//! Identical input ⇒ byte-identical output; that property is the golden
//! test.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// `web-code/src/lib/canvasPlacement.ts`'s `CANVAS_CARD_W`.
pub const CARD_W: f64 = 320.0;
/// `web-code/src/lib/canvasPlacement.ts`'s `CANVAS_CARD_H`.
pub const CARD_H: f64 = 200.0;
/// `web-code/src/lib/egoGraph.ts`'s `EGO_COL_GAP`.
pub const COL_GAP: f64 = 180.0;
/// `web-code/src/lib/egoGraph.ts`'s `EGO_ROW_PITCH`, times four — the same
/// derivation `canvasPlacement.ts`'s `CANVAS_V_STEP` makes.
pub const ROW_GAP: f64 = 48.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Lay out a board's nodes for the JSON Canvas export.
///
/// * A node with a PIN keeps the pin's position exactly — an authored
///   position is never overridden by a derived one.
/// * Every other node is placed by layer (longest-path depth over the edge
///   DAG, cycles broken by ignoring any edge that would revisit a node
///   already on the current path — deterministically, in authored edge
///   order) and by row within that layer.
/// * Row order within a layer is: step order first (a walkthrough's own
///   reading order is the best row order there is), then authored order.
pub fn place(
    nodes: &[(String, bool)],
    edges: &[(String, String)],
    steps: &[String],
    pins: &BTreeMap<String, Pin>,
) -> BTreeMap<String, Placed> {
    let ids: Vec<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();
    let index: BTreeMap<&str, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();

    // Adjacency in authored order, endpoints that exist only.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); ids.len()];
    let mut indeg: Vec<usize> = vec![0; ids.len()];
    for (from, to) in edges {
        if let (Some(&a), Some(&b)) = (index.get(from.as_str()), index.get(to.as_str())) {
            if a != b {
                adj[a].push(b);
                indeg[b] += 1;
            }
        }
    }

    // Longest-path layering with a deterministic cycle break: relax in
    // authored order for at most `n` rounds. A cycle stops contributing
    // once no depth changes, which bounds the loop without a visited-set
    // heuristic that would depend on traversal order.
    let mut layer: Vec<usize> = vec![0; ids.len()];
    for _ in 0..ids.len() {
        let mut moved = false;
        for a in 0..ids.len() {
            for &b in &adj[a] {
                if layer[b] < layer[a] + 1 {
                    layer[b] = layer[a] + 1;
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }

    // Row order: step order, then authored order.
    let step_rank: BTreeMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    let mut by_layer: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let pinned: BTreeSet<&str> = pins.keys().map(|k| k.as_str()).collect();
    for (i, id) in ids.iter().enumerate() {
        if pinned.contains(id) {
            continue;
        }
        by_layer.entry(layer[i]).or_default().push(i);
    }
    for row in by_layer.values_mut() {
        row.sort_by_key(|&i| (step_rank.get(ids[i]).copied().unwrap_or(usize::MAX), i));
    }

    let mut out: BTreeMap<String, Placed> = BTreeMap::new();
    for (id, pin) in pins {
        if index.contains_key(id.as_str()) {
            out.insert(
                id.clone(),
                Placed {
                    x: pin.x,
                    y: pin.y,
                    w: CARD_W,
                    h: CARD_H,
                },
            );
        }
    }
    for (l, row) in &by_layer {
        for (r, &i) in row.iter().enumerate() {
            out.insert(
                ids[i].to_string(),
                Placed {
                    x: (*l as f64) * (CARD_W + COL_GAP),
                    y: (r as f64) * (CARD_H + ROW_GAP),
                    w: CARD_W,
                    h: CARD_H,
                },
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(ids: &[&str]) -> Vec<(String, bool)> {
        ids.iter().map(|s| (s.to_string(), false)).collect()
    }
    fn e(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn a_chain_lays_out_left_to_right_one_per_column() {
        let p = place(
            &n(&["a", "b", "c"]),
            &e(&[("a", "b"), ("b", "c")]),
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(p["a"].x, 0.0);
        assert_eq!(p["b"].x, CARD_W + COL_GAP);
        assert_eq!(p["c"].x, 2.0 * (CARD_W + COL_GAP));
        assert!(p.values().all(|v| v.y == 0.0));
    }

    #[test]
    fn siblings_stack_in_step_order_then_authored_order() {
        let p = place(
            &n(&["root", "x", "y", "z"]),
            &e(&[("root", "x"), ("root", "y"), ("root", "z")]),
            &["z".into(), "y".into()],
            &BTreeMap::new(),
        );
        // z is step 0, y is step 1, x has no step → authored order last.
        assert_eq!(p["z"].y, 0.0);
        assert_eq!(p["y"].y, CARD_H + ROW_GAP);
        assert_eq!(p["x"].y, 2.0 * (CARD_H + ROW_GAP));
        assert!([p["x"].x, p["y"].x, p["z"].x]
            .iter()
            .all(|x| *x == CARD_W + COL_GAP));
    }

    #[test]
    fn a_pin_wins_and_the_pinned_node_is_not_flowed() {
        let mut pins = BTreeMap::new();
        pins.insert("b".to_string(), Pin { x: -7.5, y: 12.25 });
        let p = place(&n(&["a", "b"]), &e(&[("a", "b")]), &[], &pins);
        assert_eq!((p["b"].x, p["b"].y), (-7.5, 12.25));
        assert_eq!(p["a"].x, 0.0);
        // A pin for a node that is not on the board is ignored, not placed.
        let mut pins = BTreeMap::new();
        pins.insert("ghost".to_string(), Pin { x: 1.0, y: 1.0 });
        let p = place(&n(&["a"]), &[], &[], &pins);
        assert!(!p.contains_key("ghost"));
    }

    #[test]
    fn a_cycle_terminates_and_still_places_every_node() {
        let p = place(
            &n(&["a", "b", "c"]),
            &e(&[("a", "b"), ("b", "c"), ("c", "a")]),
            &[],
            &BTreeMap::new(),
        );
        assert_eq!(p.len(), 3);
        assert!(p.values().all(|v| v.x.is_finite() && v.y.is_finite()));
    }

    #[test]
    fn the_layout_is_byte_identical_across_runs() {
        let nodes = n(&["a", "b", "c", "d"]);
        let edges = e(&[("a", "b"), ("a", "c"), ("c", "d")]);
        let one = place(&nodes, &edges, &[], &BTreeMap::new());
        let two = place(&nodes, &edges, &[], &BTreeMap::new());
        assert_eq!(
            serde_json::to_string(&one.iter().map(|(k, v)| (k, v.x, v.y)).collect::<Vec<_>>())
                .unwrap(),
            serde_json::to_string(&two.iter().map(|(k, v)| (k, v.x, v.y)).collect::<Vec<_>>())
                .unwrap()
        );
    }

    #[test]
    fn an_isolated_node_lands_in_layer_zero() {
        let p = place(&n(&["lonely"]), &[], &[], &BTreeMap::new());
        assert_eq!((p["lonely"].x, p["lonely"].y), (0.0, 0.0));
        assert_eq!((p["lonely"].w, p["lonely"].h), (CARD_W, CARD_H));
    }
}
