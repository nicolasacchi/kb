//! `atlas_field` — the operator-placed atlas field: a JSON Canvas 1.0
//! sidecar codec, plus the machine-vs-operator disagreement computation.
//!
//! The dual-field atlas overlays two layouts of the same corpus: the
//! **machine field** ([`crate::atlas`]'s embedding-derived coordinates) and
//! the **operator field** — islands and artifacts a human positioned by
//! hand. Where the two disagree is the interesting part: it is the visible
//! gap between your mental map of the corpus and the model's.
//!
//! # Storage ruling (decided; do not revisit)
//!
//! The operator field is a **per-kb JSON Canvas 1.0 sidecar**, exactly like
//! `kb-server`'s boards (`routes::boards`, `web/src/lib/canvas.ts`) — a file
//! in the corpus that no `ExtensionMap` claims, so the indexer never sees
//! it: no lance row, no reindex, no `index_generation` bump (invariant
//! #15). It is **not** a reading list and **not** a board:
//!
//! * *Not a list.* `migrations/V0015__lists.sql:8-13` states the lifecycle
//!   rule in as many words — lists are **USER-CURATED** state that the
//!   indexer's unlink pass and `purge_kb_data` deliberately never touch. A
//!   field that a machine layout is compared against would want machine
//!   syncing, and a machine-synced list is forbidden by that rule. (Standing
//!   rule "one container primitive" is satisfied: this carries **no
//!   membership**. A node exists in the field because a hand placed it —
//!   full stop. Deleting a node does not remove anything from anything.)
//! * *Not a board.* A board is geometry **for one list**, keyed by
//!   `list_id`. The field is geometry for a **kb**, keyed by nothing, with
//!   no backing collection at all.
//!
//! # What this module is
//!
//! The **pure codec only** — no route, no CLI verb, no storage message, no
//! migration, and no new dependency (`serde_json` was already here). Parse
//! is tolerant in the same way `canvas.ts::parseCanvas` is tolerant: the
//! `.canvas` format is an interop contract with Obsidian and friends, so
//! anything this module does not model survives a
//! parse → serialize round-trip untouched rather than being dropped.
//!
//! # Coordinate mapping (the rounding rule, pinned)
//!
//! JSON Canvas positions live on an **integer pixel grid**; the atlas lives
//! in **`[0, 1]` unit coordinates**. The two are bridged through a fixed
//! `0..=`[`FIELD_GRID`] (0..=1000) integer grid:
//!
//! * grid → unit: `clamp(g, 0, 1000) as f32 / 1000.0`
//! * unit → grid: clamp `u` to `[0, 1]`, then `floor(u * 1000 + 0.5)` —
//!   **round half up**, ties away from zero (all inputs are non-negative
//!   after the clamp, so "half up" and "half away from zero" coincide).
//!
//! Round-trip `grid → unit → grid` is the identity for **every** integer in
//! `0..=1000` (pinned by a test that walks all 1001 of them). The reverse
//! round-trip is lossy by construction: the grid has 1001 states and `f32`
//! has far more, so `unit → grid → unit` quantises to the nearest 1/1000.
//!
//! Coordinates **outside** `0..=1000` are kept verbatim in the parsed
//! structs and re-serialized unchanged — only the *projection into unit
//! space* clamps. A hand-built canvas with negative coordinates therefore
//! round-trips losslessly even though its off-field nodes all project onto
//! the field edge.

use crate::procrustes::{self, Transform};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The integer grid the `[0, 1]` unit square maps onto. 1000 gives
/// milli-unit resolution — finer than any hand placement, coarse enough
/// that the grid → unit → grid round-trip is exact.
pub const FIELD_GRID: i64 = 1000;

/// Default node width when a canvas omits it. Matches
/// `web/src/lib/canvas.ts::DEFAULT_CARD_WIDTH` so a field and a board draw
/// cards at the same size.
pub const DEFAULT_NODE_WIDTH: i64 = 260;
/// Default node height when a canvas omits it. Matches
/// `web/src/lib/canvas.ts::DEFAULT_CARD_HEIGHT`.
pub const DEFAULT_NODE_HEIGHT: i64 = 120;

/// Grid coordinate → unit coordinate. Out-of-field values clamp to the
/// nearest edge (`0.0` / `1.0`).
pub fn unit_from_grid(g: i64) -> f32 {
    let c = g.clamp(0, FIELD_GRID);
    (c as f32) / (FIELD_GRID as f32)
}

/// Unit coordinate → grid coordinate. Clamps to `[0, 1]` first (so `NaN`,
/// infinities and off-field values all land on the field), then rounds
/// half up. See the module doc for the pinned rule.
pub fn grid_from_unit(u: f32) -> i64 {
    // NaN must be handled BEFORE `clamp`, which propagates it rather than
    // pinning it to a bound. Send it to the field origin.
    if u.is_nan() {
        return 0;
    }
    let c = u.clamp(0.0, 1.0);
    let scaled = (c as f64) * (FIELD_GRID as f64) + 0.5;
    (scaled.floor() as i64).clamp(0, FIELD_GRID)
}

/// An operator-named region — a JSON Canvas `type: "group"` node. The
/// islands are the *hand-drawn continents* of the corpus: "the stuff I
/// actually reread", "dead ends", "the 2026 rewrite". They carry no
/// membership; an artifact is "in" an island only geometrically, which is
/// exactly why this is not a collection (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Island {
    /// The JSON Canvas node id (opaque; the operator's tool assigns it).
    pub id: String,
    /// `label` — the operator's name for the region. Optional per spec.
    pub label: Option<String>,
    /// Grid coordinates, verbatim from the canvas (may be off-field).
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    /// JSON Canvas `color` (a preset "1".."6" or a hex string) — opaque.
    pub color: Option<String>,
    /// Every other key on the node object, preserved for round-trip.
    pub extra: Map<String, Value>,
}

/// A placed artifact — a JSON Canvas `type: "file"` node. `file` is the
/// artifact's **source-relative path**, matching `canvas.ts`'s file nodes
/// (which carry `ExportEntry.path` / `ListEntry.source_relative`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    /// The JSON Canvas node id (NOT the artifact id).
    pub id: String,
    /// `file` — the artifact's source-relative path. This is the join key
    /// [`disagreement`] uses; see its doc for the namespace contract.
    pub file: String,
    /// `subpath` — a `#fragment` anchor into the artifact, per spec.
    pub subpath: Option<String>,
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub color: Option<String>,
    /// Every other key on the node object, preserved for round-trip.
    pub extra: Map<String, Value>,
}

impl Island {
    /// The node's **anchor** in unit coordinates. See [`Placement::unit`]
    /// for why the anchor and not the centre.
    pub fn unit(&self) -> (f32, f32) {
        (unit_from_grid(self.x), unit_from_grid(self.y))
    }
}

impl Placement {
    /// The node's **anchor** (its `x`/`y`) in unit coordinates.
    ///
    /// Deliberately the anchor, not the card centre: the centre depends on
    /// `width`/`height`, so resizing a card would register as *moving* the
    /// artifact and show up as a disagreement the operator never made.
    pub fn unit(&self) -> (f32, f32) {
        (unit_from_grid(self.x), unit_from_grid(self.y))
    }
}

/// One node of the field, in its original document order.
///
/// [`FieldNode::Other`] is the tolerance hatch: `text` / `link` nodes, and
/// any object this module cannot model (a `group`/`file` node missing its
/// required numeric `x`/`y`, a `file` node with no `file` string), are kept
/// **verbatim** so a round-trip never destroys another tool's data. They
/// are simply invisible to [`OperatorField::islands`] /
/// [`OperatorField::placements`] and to [`disagreement`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldNode {
    Island(Island),
    Placement(Placement),
    Other(Value),
}

/// A parsed operator field.
///
/// `nodes` keeps document order (so serialize round-trips it); `extra`
/// holds every top-level key other than `nodes` — including `edges`, which
/// this module does not model but must not drop.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OperatorField {
    pub nodes: Vec<FieldNode>,
    pub extra: Map<String, Value>,
}

impl OperatorField {
    /// The operator-named regions, in document order.
    pub fn islands(&self) -> impl Iterator<Item = &Island> {
        self.nodes.iter().filter_map(|n| match n {
            FieldNode::Island(i) => Some(i),
            _ => None,
        })
    }

    /// The placed artifacts, in document order.
    pub fn placements(&self) -> impl Iterator<Item = &Placement> {
        self.nodes.iter().filter_map(|n| match n {
            FieldNode::Placement(p) => Some(p),
            _ => None,
        })
    }

    /// True when nothing at all has been placed (no islands, no
    /// placements). An empty field disagrees with nothing.
    pub fn is_empty(&self) -> bool {
        !self
            .nodes
            .iter()
            .any(|n| matches!(n, FieldNode::Island(_) | FieldNode::Placement(_)))
    }
}

/// Read a `Value` out of a node map as an `i64`, accepting a float (JSON
/// Canvas says integers, but other tools emit floats) and rounding it half
/// away from zero. `None` for a missing or non-numeric value.
fn node_int(obj: &Map<String, Value>, key: &str) -> Option<i64> {
    let n = obj.get(key)?.as_f64()?;
    if !n.is_finite() {
        return None;
    }
    // Round half away from zero without `f64::round` semantics drifting:
    // `round` IS correctly rounded here (it is not a transcendental), but
    // spelling it out keeps the rule visible next to the codec's own.
    let shifted = if n < 0.0 { n - 0.5 } else { n + 0.5 };
    Some(shifted.trunc() as i64)
}

fn node_str(obj: &Map<String, Value>, key: &str) -> Option<String> {
    obj.get(key)?.as_str().map(str::to_string)
}

/// Keys a modeled node consumes; everything else lands in `extra`.
fn extra_of(obj: &Map<String, Value>, consumed: &[&str]) -> Map<String, Value> {
    obj.iter()
        .filter(|(k, _)| !consumed.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

const ISLAND_KEYS: &[&str] = &["id", "type", "x", "y", "width", "height", "label", "color"];
const PLACEMENT_KEYS: &[&str] = &[
    "id", "type", "x", "y", "width", "height", "file", "subpath", "color",
];

fn parse_node(raw: &Value) -> Option<FieldNode> {
    let obj = raw.as_object()?;
    let id = node_str(obj, "id")?;
    let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
    let (Some(x), Some(y)) = (node_int(obj, "x"), node_int(obj, "y")) else {
        // Required by the spec; without them there is nothing to place.
        // Preserve verbatim rather than inventing a position.
        return Some(FieldNode::Other(raw.clone()));
    };
    let width = node_int(obj, "width").unwrap_or(DEFAULT_NODE_WIDTH);
    let height = node_int(obj, "height").unwrap_or(DEFAULT_NODE_HEIGHT);
    let color = node_str(obj, "color");
    match kind {
        "group" => Some(FieldNode::Island(Island {
            id,
            label: node_str(obj, "label"),
            x,
            y,
            width,
            height,
            color,
            extra: extra_of(obj, ISLAND_KEYS),
        })),
        "file" => match node_str(obj, "file") {
            Some(file) => Some(FieldNode::Placement(Placement {
                id,
                file,
                subpath: node_str(obj, "subpath"),
                x,
                y,
                width,
                height,
                color,
                extra: extra_of(obj, PLACEMENT_KEYS),
            })),
            // A `file` node with no path points at nothing.
            None => Some(FieldNode::Other(raw.clone())),
        },
        _ => Some(FieldNode::Other(raw.clone())),
    }
}

/// Parse a JSON Canvas document into an [`OperatorField`].
///
/// **Never fails.** Invalid JSON, a non-object document, or a `nodes` key
/// that is not an array all yield an empty field — the same tolerance
/// `canvas.ts::parseCanvas` has, for the same reason (a hand-editable
/// interop file must not be able to 500 a read path). Callers that need to
/// distinguish "empty" from "corrupt" should validate the JSON themselves
/// first; `kb-server`'s board route already does exactly that before
/// storing bytes.
///
/// Malformed *nodes* are skipped, not fatal: an entry that is not a JSON
/// object is dropped entirely (it is not a node in any sense), and a node
/// object this module cannot model is preserved verbatim as
/// [`FieldNode::Other`].
pub fn parse_field(canvas_json: &str) -> OperatorField {
    match serde_json::from_str::<Value>(canvas_json) {
        Ok(v) => parse_field_value(&v),
        Err(_) => OperatorField::default(),
    }
}

/// [`parse_field`] over an already-parsed JSON value.
pub fn parse_field_value(raw: &Value) -> OperatorField {
    let Some(obj) = raw.as_object() else {
        return OperatorField::default();
    };
    let nodes = obj
        .get("nodes")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(parse_node).collect::<Vec<_>>())
        .unwrap_or_default();
    OperatorField {
        nodes,
        extra: extra_of(obj, &["nodes"]),
    }
}

fn island_to_value(i: &Island) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), Value::String(i.id.clone()));
    m.insert("type".into(), Value::String("group".into()));
    m.insert("x".into(), Value::from(i.x));
    m.insert("y".into(), Value::from(i.y));
    m.insert("width".into(), Value::from(i.width));
    m.insert("height".into(), Value::from(i.height));
    if let Some(label) = &i.label {
        m.insert("label".into(), Value::String(label.clone()));
    }
    if let Some(color) = &i.color {
        m.insert("color".into(), Value::String(color.clone()));
    }
    for (k, v) in &i.extra {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

fn placement_to_value(p: &Placement) -> Value {
    let mut m = Map::new();
    m.insert("id".into(), Value::String(p.id.clone()));
    m.insert("type".into(), Value::String("file".into()));
    m.insert("x".into(), Value::from(p.x));
    m.insert("y".into(), Value::from(p.y));
    m.insert("width".into(), Value::from(p.width));
    m.insert("height".into(), Value::from(p.height));
    m.insert("file".into(), Value::String(p.file.clone()));
    if let Some(sp) = &p.subpath {
        m.insert("subpath".into(), Value::String(sp.clone()));
    }
    if let Some(color) = &p.color {
        m.insert("color".into(), Value::String(color.clone()));
    }
    for (k, v) in &p.extra {
        m.insert(k.clone(), v.clone());
    }
    Value::Object(m)
}

/// Serialize an [`OperatorField`] back to a JSON Canvas document.
///
/// `nodes` is always emitted (an empty field serializes to
/// `{"nodes":[]}` plus whatever `extra` carried), in the document order the
/// parse preserved. Round-trip fidelity is **semantic, not byte-for-byte**:
/// `serde_json`'s `Map` is a `BTreeMap` here (no `preserve_order` feature
/// in this workspace), so object keys come back sorted and whitespace is
/// normalised. Every *value* survives.
pub fn serialize_field(field: &OperatorField) -> String {
    let mut root = field.extra.clone();
    let nodes: Vec<Value> = field
        .nodes
        .iter()
        .map(|n| match n {
            FieldNode::Island(i) => island_to_value(i),
            FieldNode::Placement(p) => placement_to_value(p),
            FieldNode::Other(v) => v.clone(),
        })
        .collect();
    root.insert("nodes".into(), Value::Array(nodes));
    Value::Object(root).to_string()
}

// --- disagreement --------------------------------------------------------

/// One machine-laid-out artifact: the atlas's own coordinates, in unit
/// space. `id` must be in the **same namespace** as the operator field's
/// `file` values — source-relative paths (see [`disagreement`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachinePoint {
    pub id: String,
    pub x: f32,
    pub y: f32,
}

/// How far one artifact sits from where the operator put it, after the two
/// fields have been brought into a common frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Disagreement {
    /// The join key (a source-relative path).
    pub id: String,
    /// The machine's unit coordinates, untouched.
    pub machine: (f32, f32),
    /// The operator's position **after** the Procrustes alignment — i.e.
    /// in the machine's frame, directly comparable to `machine`.
    pub operator: (f32, f32),
    /// The operator's position **as placed**, before alignment. Kept so a
    /// UI can draw the raw field as well as the aligned one.
    pub operator_raw: (f32, f32),
    /// Euclidean distance between `machine` and `operator`.
    pub distance: f32,
}

/// The id-joined point pairs, index-aligned across all three vectors:
/// `ids[i]` was placed by the operator at `from[i]` and laid out by the
/// machine at `to[i]`, all in unit coordinates.
struct JoinedPairs {
    ids: Vec<String>,
    /// Operator positions — the `from` side of the [`procrustes`] fit.
    from: Vec<(f32, f32)>,
    /// Machine positions — the `to` side (the frame everything lands in).
    to: Vec<(f32, f32)>,
}

/// Collect the id-joined point pairs, in a deterministic order.
///
/// * Join key: the placement's `file`. Ids present in only one of the two
///   sets are **dropped** — never invented, never persisted.
/// * Duplicates: the FIRST occurrence wins on both sides (document order
///   for placements, slice order for machine points). Placing the same
///   artifact twice is legal in JSON Canvas; the field just uses the first
///   placement as that artifact's position.
/// * Order: the joined ids are walked in **sorted** order, not in the
///   caller's slice order, so the floating-point accumulation inside
///   [`procrustes::align`] does not depend on how the caller happened to
///   order its machine points.
fn joined_pairs(machine: &[MachinePoint], operator: &OperatorField) -> JoinedPairs {
    let mut op: BTreeMap<&str, (f32, f32)> = BTreeMap::new();
    for p in operator.placements() {
        op.entry(p.file.as_str()).or_insert_with(|| p.unit());
    }
    let mut mach: BTreeMap<&str, (f32, f32)> = BTreeMap::new();
    for m in machine {
        if !m.x.is_finite() || !m.y.is_finite() {
            continue;
        }
        mach.entry(m.id.as_str()).or_insert((m.x, m.y));
    }
    let mut ids = Vec::new();
    let mut from = Vec::new();
    let mut to = Vec::new();
    for (id, o) in &op {
        if let Some(m) = mach.get(id) {
            ids.push((*id).to_string());
            from.push(*o);
            to.push(*m);
        }
    }
    JoinedPairs { ids, from, to }
}

/// Fit the transform that brings the operator field into the machine's
/// frame. Exposed so a renderer can draw the whole operator field (islands
/// included) overlaid on the machine atlas using the same alignment
/// [`disagreement`] scored with. Degenerate joins (no shared ids, one
/// shared id, everything coincident) return a finite, invertible transform
/// — see [`procrustes::align`].
pub fn field_alignment(machine: &[MachinePoint], operator: &OperatorField) -> Transform {
    let j = joined_pairs(machine, operator);
    procrustes::align(&j.from, &j.to)
}

/// Per-artifact displacement between the machine layout and the operator's
/// field, **largest disagreement first**.
///
/// The operator's points are Procrustes-aligned onto the machine's first
/// ([`procrustes::align`]), so the result measures *relative* disagreement
/// — the operator is not being penalised for having drawn their map at a
/// different rotation, scale or offset. Only what is left after that is
/// reported.
///
/// # Join namespace
///
/// `machine[i].id` is matched against [`Placement::file`], i.e.
/// **source-relative paths**, which is what `canvas.ts` writes into a file
/// node. A caller holding artifact ids must map them to source-relative
/// paths first (the server has both on `DocSummary`). An id on only one
/// side is dropped: the field never invents a position for an artifact the
/// operator did not place, and never reports one for a placement whose
/// artifact has left the index.
///
/// # Ordering
///
/// Descending by `distance`, ties broken by ascending `id` — a total,
/// input-order-independent order, so two runs (and two machines) produce
/// the same list.
///
/// # The fit is global: one outlier moves everybody
///
/// [`procrustes::align`] is plain least squares with no robust loss and no
/// outlier rejection, so a single wildly-misplaced doc pulls the whole
/// frame by roughly `1/n`. On a small join that pull is large enough that
/// the docs the operator never touched can out-score the one they moved
/// (a 4-doc join with one doc relocated across the map is enough to invert
/// the ranking). This is a property of the estimator, not a defect: the
/// numbers are *relative* disagreement, and on a real corpus — hundreds of
/// placements — `1/n` is noise. Do not "fix" it with a trimmed/iterative
/// re-fit: iterate-and-discard is a fitting procedure whose result depends
/// on its own convergence, which is exactly the kind of non-reproducibility
/// [`crate::procrustes`]'s determinism contract exists to keep out.
pub fn disagreement(machine: &[MachinePoint], operator: &OperatorField) -> Vec<Disagreement> {
    let JoinedPairs { ids, from, to } = joined_pairs(machine, operator);
    if ids.is_empty() {
        return Vec::new();
    }
    let t = procrustes::align(&from, &to);
    let aligned = procrustes::apply(&t, &from);
    let mut out: Vec<Disagreement> = ids
        .into_iter()
        .zip(from)
        .zip(aligned)
        .zip(to)
        .map(|(((id, raw), op), m)| {
            let dx = op.0 - m.0;
            let dy = op.1 - m.1;
            // sqrt only — no `hypot` (libm-defined; see procrustes' doc).
            let distance = (dx * dx + dy * dy).sqrt();
            Disagreement {
                id,
                machine: m,
                operator: op,
                operator_raw: raw,
                distance,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.distance
            .total_cmp(&a.distance)
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(nodes: &str) -> String {
        format!(r#"{{"nodes":[{nodes}],"edges":[]}}"#)
    }

    fn file_node(id: &str, file: &str, x: i64, y: i64) -> String {
        format!(
            r#"{{"id":"{id}","type":"file","x":{x},"y":{y},"width":100,"height":50,"file":"{file}"}}"#
        )
    }

    // --- coordinate grid ---------------------------------------------

    #[test]
    fn grid_unit_round_trips_for_every_grid_value() {
        // The pinned rounding rule: grid → unit → grid is the identity on
        // ALL 1001 states of the field grid, edges included.
        for g in 0..=FIELD_GRID {
            assert_eq!(grid_from_unit(unit_from_grid(g)), g, "grid value {g}");
        }
    }

    #[test]
    fn grid_edges_map_to_unit_edges() {
        assert_eq!(unit_from_grid(0), 0.0);
        assert_eq!(unit_from_grid(FIELD_GRID), 1.0);
        assert_eq!(grid_from_unit(0.0), 0);
        assert_eq!(grid_from_unit(1.0), FIELD_GRID);
    }

    #[test]
    fn off_field_coordinates_clamp_on_projection_only() {
        assert_eq!(unit_from_grid(-5000), 0.0);
        assert_eq!(unit_from_grid(9_999), 1.0);
        assert_eq!(grid_from_unit(-3.0), 0);
        assert_eq!(grid_from_unit(42.0), FIELD_GRID);
        assert_eq!(grid_from_unit(f32::NAN), 0);
        assert_eq!(grid_from_unit(f32::INFINITY), FIELD_GRID);
        assert_eq!(grid_from_unit(f32::NEG_INFINITY), 0);
    }

    #[test]
    fn unit_to_grid_rounds_half_up() {
        // EXACT ties. No `(k + 0.5) / 1000` is representable in binary
        // (1000 = 8·125), so a decimal "half" like 0.1235 is never really
        // a tie — it lands a hair either side and the rule never fires.
        // `m / 16` for odd `m` IS exact in f32 and `m · 1000 / 16` lands
        // dead on `k + 0.5`; those are the only true ties, and each must
        // round UP.
        assert_eq!(grid_from_unit(0.0625), 63); // 62.5 → 63
        assert_eq!(grid_from_unit(0.1875), 188); // 187.5 → 188
        assert_eq!(grid_from_unit(0.9375), 938); // 937.5 → 938
                                                 // Away from a tie the rule is plain nearest-integer.
        assert_eq!(grid_from_unit(0.5), 500);
        assert_eq!(grid_from_unit(0.1234), 123);
        assert_eq!(grid_from_unit(0.1236), 124);
        // The nearest f32 to 0.0005 sits just ABOVE 1/2000, so it rounds
        // up on its own merit rather than on the half-up rule. Pinned
        // because the value is deterministic and someone will assume it
        // is the tie case.
        assert_eq!(grid_from_unit(0.0005), 1);
        assert_eq!(grid_from_unit(0.0004), 0);
    }

    #[test]
    fn off_field_node_coordinates_survive_round_trip_unclamped() {
        let json = canvas(&file_node("a", "docs/a.html", -400, 5000));
        let f = parse_field(&json);
        let p = f.placements().next().unwrap();
        assert_eq!((p.x, p.y), (-400, 5000), "raw coords must not be clamped");
        assert_eq!(p.unit(), (0.0, 1.0), "projection clamps");
        let back = parse_field(&serialize_field(&f));
        assert_eq!(back, f);
    }

    // --- codec --------------------------------------------------------

    #[test]
    fn round_trip_preserves_islands_placements_and_unknown_fields() {
        let json = r##"{
            "nodes": [
                {"id":"g1","type":"group","x":0,"y":0,"width":400,"height":300,
                 "label":"the stuff I reread","color":"3","styleAttributes":{"z":9}},
                {"id":"n1","type":"file","x":100,"y":250,"width":260,"height":120,
                 "file":"research/atlas.html","subpath":"#section-3","fromFuture":true},
                {"id":"t1","type":"text","x":10,"y":10,"width":50,"height":50,
                 "text":"a note"}
            ],
            "edges": [{"id":"e1","fromNode":"g1","toNode":"n1"}],
            "kbFieldVersion": 1
        }"##;
        let f = parse_field(json);
        assert_eq!(f.islands().count(), 1);
        assert_eq!(f.placements().count(), 1);
        assert_eq!(f.nodes.len(), 3, "the text node is preserved as Other");

        let island = f.islands().next().unwrap();
        assert_eq!(island.label.as_deref(), Some("the stuff I reread"));
        assert_eq!(island.color.as_deref(), Some("3"));
        assert!(island.extra.contains_key("styleAttributes"));

        let p = f.placements().next().unwrap();
        assert_eq!(p.file, "research/atlas.html");
        assert_eq!(p.subpath.as_deref(), Some("#section-3"));
        assert_eq!(p.extra.get("fromFuture"), Some(&Value::Bool(true)));

        // Top-level unknowns (and `edges`, which we never model) survive.
        let out = serialize_field(&f);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["kbFieldVersion"], Value::from(1));
        assert_eq!(v["edges"][0]["id"], Value::from("e1"));

        // And the whole thing is a fixed point: parse(serialize(x)) == x.
        assert_eq!(parse_field(&out), f);
        assert_eq!(serialize_field(&parse_field(&out)), out);
    }

    #[test]
    fn malformed_input_is_never_fatal() {
        for bad in [
            "",
            "not json",
            "[1,2,3]",
            "null",
            "42",
            r#"{"nodes":"oops"}"#,
            r#"{"nodes":null}"#,
        ] {
            let f = parse_field(bad);
            assert!(f.is_empty(), "{bad:?} should parse to an empty field");
            assert_eq!(f.placements().count(), 0);
            assert_eq!(f.islands().count(), 0);
        }
    }

    #[test]
    fn malformed_nodes_are_skipped_not_fatal() {
        let json = r#"{"nodes":[
            17,
            "a string",
            null,
            {"type":"file","x":1,"y":2,"file":"no/id.html"},
            {"id":"no-coords","type":"file","file":"a.html"},
            {"id":"no-file","type":"file","x":1,"y":2},
            {"id":"ok","type":"file","x":3,"y":4,"file":"good.html"}
        ]}"#;
        let f = parse_field(json);
        // Non-objects (and the id-less node) are dropped entirely; the two
        // unmodellable objects are preserved verbatim.
        assert_eq!(f.placements().count(), 1);
        assert_eq!(f.placements().next().unwrap().file, "good.html");
        assert_eq!(f.nodes.len(), 3, "2 Other + 1 Placement");
        let others = f
            .nodes
            .iter()
            .filter(|n| matches!(n, FieldNode::Other(_)))
            .count();
        assert_eq!(others, 2);
    }

    #[test]
    fn empty_field_round_trips_and_is_empty() {
        let f = parse_field("{}");
        assert!(f.is_empty());
        assert_eq!(serialize_field(&f), r#"{"nodes":[]}"#);
        assert_eq!(parse_field(r#"{"nodes":[]}"#), f);
        assert_eq!(f, OperatorField::default());
        assert_eq!(
            serialize_field(&OperatorField::default()),
            r#"{"nodes":[]}"#
        );
    }

    #[test]
    fn missing_width_height_default_to_the_canvas_card_size() {
        let json = r#"{"nodes":[{"id":"n","type":"file","x":1,"y":2,"file":"a.html"}]}"#;
        let p = parse_field(json).placements().next().cloned().unwrap();
        assert_eq!(p.width, DEFAULT_NODE_WIDTH);
        assert_eq!(p.height, DEFAULT_NODE_HEIGHT);
    }

    #[test]
    fn float_coordinates_are_rounded_half_away_from_zero() {
        let json = r#"{"nodes":[
            {"id":"a","type":"file","x":10.5,"y":-10.5,"file":"a.html"},
            {"id":"b","type":"file","x":10.4,"y":-10.4,"file":"b.html"}
        ]}"#;
        let ps: Vec<_> = parse_field(json).placements().cloned().collect();
        assert_eq!((ps[0].x, ps[0].y), (11, -11));
        assert_eq!((ps[1].x, ps[1].y), (10, -10));
    }

    #[test]
    fn island_without_a_label_round_trips_without_inventing_one() {
        let json = r#"{"nodes":[{"id":"g","type":"group","x":0,"y":0,"width":10,"height":10}]}"#;
        let f = parse_field(json);
        assert_eq!(f.islands().next().unwrap().label, None);
        let v: Value = serde_json::from_str(&serialize_field(&f)).unwrap();
        assert!(v["nodes"][0].get("label").is_none());
        assert_eq!(v["nodes"][0]["type"], Value::from("group"));
    }

    #[test]
    fn is_empty_ignores_unmodelled_nodes() {
        let json = r#"{"nodes":[{"id":"t","type":"text","x":0,"y":0,"text":"hi"}]}"#;
        let f = parse_field(json);
        assert_eq!(f.nodes.len(), 1);
        assert!(f.is_empty(), "a text-only canvas places nothing");
    }

    // --- disagreement --------------------------------------------------

    #[test]
    fn disagreement_is_empty_without_a_join() {
        let f = parse_field(&canvas(&file_node("n1", "a.html", 0, 0)));
        assert!(disagreement(&[], &f).is_empty());
        let machine = vec![MachinePoint {
            id: "b.html".into(),
            x: 0.5,
            y: 0.5,
        }];
        assert!(
            disagreement(&machine, &f).is_empty(),
            "no shared id → nothing to report"
        );
        assert!(disagreement(&machine, &OperatorField::default()).is_empty());
    }

    #[test]
    fn ids_present_on_only_one_side_are_dropped() {
        let nodes = [
            file_node("n1", "a.html", 0, 0),
            file_node("n2", "b.html", 500, 0),
            file_node("n3", "operator-only.html", 1000, 1000),
        ]
        .join(",");
        let f = parse_field(&canvas(&nodes));
        let machine = vec![
            MachinePoint {
                id: "a.html".into(),
                x: 0.0,
                y: 0.0,
            },
            MachinePoint {
                id: "b.html".into(),
                x: 0.5,
                y: 0.0,
            },
            MachinePoint {
                id: "machine-only.html".into(),
                x: 0.9,
                y: 0.9,
            },
        ];
        let d = disagreement(&machine, &f);
        let ids: Vec<&str> = d.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, vec!["a.html", "b.html"]);
    }

    #[test]
    fn a_purely_rotated_field_registers_near_zero_disagreement() {
        // The operator drew the same map, rotated 90° and shifted. After
        // alignment there is nothing left to report.
        let machine_pts = [(0.1f32, 0.1f32), (0.9, 0.2), (0.4, 0.8), (0.6, 0.35)];
        let nodes: Vec<String> = machine_pts
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| {
                // rotate 90° CCW about the origin then shift into field
                let (rx, ry) = (-y, x);
                file_node(
                    &format!("n{i}"),
                    &format!("d{i}.html"),
                    grid_from_unit(rx + 0.9),
                    grid_from_unit(ry),
                )
            })
            .collect();
        let f = parse_field(&canvas(&nodes.join(",")));
        let machine: Vec<MachinePoint> = machine_pts
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| MachinePoint {
                id: format!("d{i}.html"),
                x,
                y,
            })
            .collect();
        let d = disagreement(&machine, &f);
        assert_eq!(d.len(), 4);
        for row in &d {
            assert!(
                row.distance < 2e-3,
                "{} should align away (got {})",
                row.id,
                row.distance
            );
        }
    }

    #[test]
    fn one_moved_doc_sorts_to_the_top() {
        // Eight docs, one filed somewhere else. The fit is GLOBAL least
        // squares, so the outlier drags the whole frame by roughly 1/n —
        // with n = 8 that drag is small enough that the doc the operator
        // actually moved is still the one that leads. (With n = 4 it is
        // NOT: a quarter of the frame's pull is a big rotation/scale
        // kick, and the innocent docs can out-score the moved one. That
        // is a real property of the estimator, documented on
        // `disagreement`, not a bug — hence eight points here.)
        let machine_pts = [
            (0.1f32, 0.1f32),
            (0.3, 0.15),
            (0.5, 0.1),
            (0.7, 0.2),
            (0.9, 0.3),
            (0.8, 0.6),
            (0.5, 0.8),
            (0.2, 0.7),
        ];
        let mut placed: Vec<(f32, f32)> = machine_pts.to_vec();
        placed[3] = (0.15, 0.85); // the operator filed d3 across the map
        let nodes: Vec<String> = placed
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| {
                file_node(
                    &format!("n{i}"),
                    &format!("d{i}.html"),
                    grid_from_unit(x),
                    grid_from_unit(y),
                )
            })
            .collect();
        let f = parse_field(&canvas(&nodes.join(",")));
        let machine: Vec<MachinePoint> = machine_pts
            .iter()
            .enumerate()
            .map(|(i, &(x, y))| MachinePoint {
                id: format!("d{i}.html"),
                x,
                y,
            })
            .collect();
        let d = disagreement(&machine, &f);
        assert_eq!(d.len(), 8);
        assert_eq!(d[0].id, "d3.html", "the moved doc leads: {d:?}");
        for w in d.windows(2) {
            assert!(w[0].distance >= w[1].distance, "not sorted desc: {d:?}");
        }
    }

    #[test]
    fn equal_distances_break_the_tie_on_ascending_id() {
        // `zz` and `aa` are CO-LOCATED on both sides — same operator
        // coordinates, same machine coordinates — so their distances run
        // through bit-for-bit identical arithmetic whatever the fitted
        // transform turns out to be. That is the only way to construct a
        // guaranteed f32 tie; a "symmetric" pair is not one (mirrored
        // fixtures are fitted EXACTLY by the reflection branch, which
        // collapses every distance to 0.0 and makes the assertion vacuous).
        let nodes = [
            file_node("n1", "zz.html", 100, 100),
            file_node("n2", "aa.html", 100, 100),
            file_node("n3", "mm.html", 900, 900),
            file_node("n4", "nn.html", 100, 900),
        ]
        .join(",");
        let f = parse_field(&canvas(&nodes));
        let machine = vec![
            MachinePoint {
                id: "zz.html".into(),
                x: 0.2,
                y: 0.2,
            },
            MachinePoint {
                id: "aa.html".into(),
                x: 0.2,
                y: 0.2,
            },
            MachinePoint {
                id: "mm.html".into(),
                x: 0.9,
                y: 0.9,
            },
            MachinePoint {
                id: "nn.html".into(),
                x: 0.1,
                y: 0.9,
            },
        ];
        let d = disagreement(&machine, &f);
        assert_eq!(d.len(), 4);
        let pos = |id: &str| d.iter().position(|x| x.id == id).unwrap();
        let (ia, iz) = (pos("aa.html"), pos("zz.html"));
        assert_eq!(
            d[ia].distance.to_bits(),
            d[iz].distance.to_bits(),
            "co-located docs must score identically: {d:?}"
        );
        assert!(
            d[ia].distance > 0.0,
            "the tie must not be a trivial 0: {d:?}"
        );
        assert_eq!(iz, ia + 1, "tied rows must be adjacent: {d:?}");
        assert!(
            d[ia].id < d[iz].id,
            "ties must resolve on ascending id: {d:?}"
        );
    }

    #[test]
    fn result_is_independent_of_machine_slice_order() {
        let nodes = [
            file_node("n1", "a.html", 0, 0),
            file_node("n2", "b.html", 700, 100),
            file_node("n3", "c.html", 200, 900),
            file_node("n4", "d.html", 950, 950),
        ]
        .join(",");
        let f = parse_field(&canvas(&nodes));
        let mut machine = vec![
            MachinePoint {
                id: "a.html".into(),
                x: 0.05,
                y: 0.1,
            },
            MachinePoint {
                id: "b.html".into(),
                x: 0.8,
                y: 0.05,
            },
            MachinePoint {
                id: "c.html".into(),
                x: 0.1,
                y: 0.7,
            },
            MachinePoint {
                id: "d.html".into(),
                x: 0.99,
                y: 0.85,
            },
        ];
        let a = disagreement(&machine, &f);
        machine.reverse();
        let b = disagreement(&machine, &f);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(&b) {
            assert_eq!(x.id, y.id);
            assert_eq!(
                x.distance.to_bits(),
                y.distance.to_bits(),
                "reordering the machine slice changed the fit"
            );
        }
    }

    #[test]
    fn duplicate_placements_take_the_first_in_document_order() {
        let nodes = [
            file_node("n1", "a.html", 0, 0),
            file_node("n2", "a.html", 1000, 1000),
            file_node("n3", "b.html", 500, 500),
        ]
        .join(",");
        let f = parse_field(&canvas(&nodes));
        let machine = vec![
            MachinePoint {
                id: "a.html".into(),
                x: 0.0,
                y: 0.0,
            },
            MachinePoint {
                id: "b.html".into(),
                x: 0.5,
                y: 0.5,
            },
        ];
        let d = disagreement(&machine, &f);
        assert_eq!(d.len(), 1 + 1);
        let a = d.iter().find(|x| x.id == "a.html").unwrap();
        assert_eq!(a.operator_raw, (0.0, 0.0), "first placement wins");
    }

    #[test]
    fn field_alignment_of_a_degenerate_join_is_finite() {
        // One shared id → pure translation; no shared ids → identity.
        let f = parse_field(&canvas(&file_node("n1", "a.html", 250, 250)));
        let machine = vec![MachinePoint {
            id: "a.html".into(),
            x: 0.9,
            y: 0.9,
        }];
        let t = field_alignment(&machine, &f);
        assert!(t.is_finite());
        assert_eq!(t.scale, 1.0);
        let d = disagreement(&machine, &f);
        assert_eq!(d.len(), 1);
        assert!(d[0].distance < 1e-5, "a lone pair aligns exactly");

        let empty = field_alignment(&[], &OperatorField::default());
        assert_eq!(empty, Transform::IDENTITY);
    }

    #[test]
    fn non_finite_machine_points_are_dropped() {
        let nodes = [
            file_node("n1", "a.html", 0, 0),
            file_node("n2", "b.html", 500, 500),
        ]
        .join(",");
        let f = parse_field(&canvas(&nodes));
        let machine = vec![
            MachinePoint {
                id: "a.html".into(),
                x: f32::NAN,
                y: 0.0,
            },
            MachinePoint {
                id: "b.html".into(),
                x: 0.5,
                y: 0.5,
            },
        ];
        let d = disagreement(&machine, &f);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].id, "b.html");
        assert!(d[0].distance.is_finite());
    }

    #[test]
    fn disagreement_serializes_for_a_future_wire() {
        let d = Disagreement {
            id: "a.html".into(),
            machine: (0.1, 0.2),
            operator: (0.3, 0.4),
            operator_raw: (0.5, 0.6),
            distance: 0.28,
        };
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(serde_json::from_str::<Disagreement>(&json).unwrap(), d);
    }
}
