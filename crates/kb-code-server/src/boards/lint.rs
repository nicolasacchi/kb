//! V74-L1 — the `canvas apply` LINT.
//!
//! D10 names it in one clause ("an apply lint (node cap, disconnected
//! components, steps required above N nodes)"); this module is that clause,
//! plus the rules that fall out of the rest of the design — above all the
//! COORDINATE refusal, which is the one rule whose whole job is to teach an
//! LLM what a board is not.
//!
//! # Two severities, and the line between them
//!
//! * **`refuse`** — the document is not applied at all, and NOTHING is
//!   written. Reserved for what is structurally wrong: a cap exceeded, an
//!   unknown vocabulary value, an edge pointing at a node that is not in
//!   the document, a MALFORMED address.
//! * **`warn`** — the document is applied and the report says what will be
//!   less than it looks. Reserved for what is merely absent: a reference
//!   whose target does not currently resolve (the node becomes an honest
//!   orphan — dropping it would be the one thing this whole unit exists to
//!   prevent), a disconnected component under the cap, a thread id nothing
//!   answers to.
//!
//! The split is the difference between "I cannot read this" and "I read
//! this and it points at something that is gone", and conflating them is
//! what turns a board into either a liar or a brick wall.
//!
//! # Why the report is a LIST, not a first error
//!
//! An agent authoring a board retries the whole document. `serde`'s
//! first-error-wins message costs one round trip per mistake; a full report
//! costs one. That is also why [`super::RefFields`] is a flat optional-field
//! struct rather than a `#[serde(tag = "kind")]` enum — see its own doc.

use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// The key names an LLM reaches for when it wants to place a card. Any of
/// them, ANYWHERE in the document except inside the `pins` map, is the
/// coordinate refusal. Checked over the RAW JSON before the typed parse, so
/// the caller gets this message rather than serde's generic "unknown field".
pub const COORDINATE_KEYS: [&str; 10] = [
    "x", "y", "w", "h", "width", "height", "position", "left", "top", "coords",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Refuse,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// A stable rule id — the thing an agent can branch on. Never
    /// reworded across a release without changing the id.
    pub rule: &'static str,
    pub severity: Severity,
    /// Which node/edge/step the finding is about, when it is about one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub message: String,
}

impl Finding {
    fn refuse(rule: &'static str, at: Option<String>, message: impl Into<String>) -> Self {
        Self {
            rule,
            severity: Severity::Refuse,
            at,
            message: message.into(),
        }
    }
    fn warn(rule: &'static str, at: Option<String>, message: impl Into<String>) -> Self {
        Self {
            rule,
            severity: Severity::Warn,
            at,
            message: message.into(),
        }
    }
}

/// The whole lint report. `refused` is DERIVED (`findings` contains a
/// `Refuse`), never set independently — two fields that can disagree about
/// the same fact is exactly the drift this codebase keeps designing out.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Report {
    pub findings: Vec<Finding>,
    /// Weakly-connected components of the node graph, each listed by node
    /// id, in first-appearance order. Always present — a one-component
    /// board reports one component rather than an empty list.
    pub components: Vec<Vec<String>>,
}

impl Report {
    pub fn refused(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Refuse)
    }
    pub fn refusals(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Refuse)
    }
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warn)
    }
    /// The one-line refusal an HTTP 400 / a CLI exit carries.
    pub fn refusal_summary(&self) -> String {
        let msgs: Vec<String> = self
            .refusals()
            .map(|f| match &f.at {
                Some(at) => format!("[{}] {at}: {}", f.rule, f.message),
                None => format!("[{}] {}", f.rule, f.message),
            })
            .collect();
        format!(
            "canvas apply refused by {} lint rule(s): {}",
            msgs.len(),
            msgs.join(" · ")
        )
    }
}

/// Caller-supplied relaxations. Deliberately tiny: every flag here is a
/// documented escape from ONE rule, never a blanket `--force`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Opts {
    /// Allow more than [`MAX_COMPONENTS`] weakly-connected components.
    pub allow_disconnected: bool,
}

/// The RAW-JSON pre-pass: the coordinate refusal, plus the two shape checks
/// that must happen before a typed parse can even be attempted.
///
/// Returns the findings; an empty vec means "go ahead and parse".
pub fn precheck_raw(raw: &serde_json::Value) -> Vec<Finding> {
    let mut out = Vec::new();
    scan_for_coordinates(raw, "", &mut out);
    out
}

fn scan_for_coordinates(v: &serde_json::Value, path: &str, out: &mut Vec<Finding>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, child) in map {
                // `pins` is the ONE sanctioned home for geometry, and its
                // own `{x, y}` is not a violation — so the walk stops there.
                if path.is_empty() && k == "pins" {
                    continue;
                }
                if COORDINATE_KEYS.contains(&k.as_str()) {
                    out.push(Finding::refuse(
                        "coordinates",
                        Some(if path.is_empty() {
                            k.clone()
                        } else {
                            format!("{path}.{k}")
                        }),
                        format!(
                            "boards are coordinate-free; {k:?} is geometry and the layout \
                             engine owns geometry. Use the top-level `pins` map \
                             (`\"pins\": {{\"<node id>\": {{\"x\": 0, \"y\": 0}}}}`) for a \
                             node whose position a human fixed by hand"
                        ),
                    ));
                }
                let child_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                scan_for_coordinates(child, &child_path, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                scan_for_coordinates(child, &format!("{path}[{i}]"), out);
            }
        }
        _ => {}
    }
}

/// The full lint over a parsed document. PURE: no store, no filesystem, no
/// clock. Every rule here is syntactic — "does this document describe a
/// board at all" — and the semantic half (does this reference resolve
/// TODAY) is [`super::resolve`]'s, run afterwards and reported as warnings.
pub fn check(doc: &BoardDoc, opts: Opts) -> Report {
    let mut f: Vec<Finding> = Vec::new();

    if doc.schema != SCHEMA {
        f.push(Finding::refuse(
            "schema",
            None,
            format!("schema must be {SCHEMA:?}, got {:?}", doc.schema),
        ));
    }
    if !is_valid_id(&doc.slug) {
        f.push(Finding::refuse(
            "slug",
            Some(doc.slug.clone()),
            format!(
                "a board slug is 1..={MAX_ID_LEN} chars of [a-z0-9_-] starting with an \
                 alphanumeric (it rides a URL path segment)"
            ),
        ));
    }
    let title = doc.title.trim();
    if title.is_empty() {
        f.push(Finding::refuse("title", None, "title must be non-empty"));
    } else if doc.title.chars().count() > MAX_TITLE_LEN {
        f.push(Finding::refuse(
            "title",
            None,
            format!(
                "title is {} chars; cap is {MAX_TITLE_LEN}",
                doc.title.chars().count()
            ),
        ));
    }
    if doc.description_md.len() > MAX_DESCRIPTION_BYTES {
        f.push(Finding::refuse(
            "body-cap",
            None,
            format!(
                "description_md is {} bytes; cap is {MAX_DESCRIPTION_BYTES}",
                doc.description_md.len()
            ),
        ));
    }
    if let Some(s) = &doc.status {
        if !APPLIABLE_STATUSES.contains(&s.as_str()) {
            f.push(Finding::refuse(
                "status",
                None,
                format!(
                    "apply may only write {} — {:?} is a TRANSITION, reached through \
                     `kb-code canvas accept`/`archive` (loopback), never authored in a \
                     document",
                    APPLIABLE_STATUSES.join(" or "),
                    s
                ),
            ));
        }
    }

    // --- caps, before anything walks the collections -----------------
    if doc.nodes.len() > MAX_NODES {
        f.push(Finding::refuse(
            "node-cap",
            None,
            format!(
                "{} nodes; the cap is {MAX_NODES}. Split the board, or collapse detail \
                 into groups",
                doc.nodes.len()
            ),
        ));
    }
    if doc.edges.len() > MAX_EDGES {
        f.push(Finding::refuse(
            "edge-cap",
            None,
            format!("{} edges; the cap is {MAX_EDGES}", doc.edges.len()),
        ));
    }

    // --- nodes --------------------------------------------------------
    let mut ids: BTreeSet<&str> = BTreeSet::new();
    let mut kinds: BTreeMap<&str, &str> = BTreeMap::new();
    for n in &doc.nodes {
        if !is_valid_id(&n.id) {
            f.push(Finding::refuse(
                "node-id",
                Some(n.id.clone()),
                format!(
                    "a node id is 1..={MAX_ID_LEN} chars of [a-z0-9_-] starting with an \
                     alphanumeric"
                ),
            ));
            continue;
        }
        if !ids.insert(n.id.as_str()) {
            f.push(Finding::refuse(
                "node-id",
                Some(n.id.clone()),
                "duplicate node id — nodes are keyed by id, so a duplicate would make \
                 the upsert ambiguous",
            ));
            continue;
        }
        if !is_valid_node_kind(&n.kind) {
            f.push(Finding::refuse(
                "node-kind",
                Some(n.id.clone()),
                format!(
                    "unknown node kind {:?} — the vocabulary is {}",
                    n.kind,
                    NODE_KINDS.join(", ")
                ),
            ));
            continue;
        }
        kinds.insert(n.id.as_str(), n.kind.as_str());
        if let Some(b) = &n.body_md {
            if b.len() > MAX_BODY_BYTES {
                f.push(Finding::refuse(
                    "body-cap",
                    Some(n.id.clone()),
                    format!("body_md is {} bytes; cap is {MAX_BODY_BYTES}", b.len()),
                ));
            }
        }
        if let Some(t) = &n.title {
            if t.chars().count() > MAX_TITLE_LEN {
                f.push(Finding::refuse(
                    "title",
                    Some(n.id.clone()),
                    format!(
                        "node title is {} chars; cap is {MAX_TITLE_LEN}",
                        t.chars().count()
                    ),
                ));
            }
        }
        check_reference(n, &mut f);
    }

    // group membership: both directions
    for n in &doc.nodes {
        if let Some(g) = &n.group {
            match kinds.get(g.as_str()) {
                None => f.push(Finding::refuse(
                    "group-member",
                    Some(n.id.clone()),
                    format!("group {g:?} is not a node in this document"),
                )),
                Some(k) if *k != KIND_GROUP => f.push(Finding::refuse(
                    "group-member",
                    Some(n.id.clone()),
                    format!("group {g:?} is a {k:?} node, not a group"),
                )),
                _ => {}
            }
        }
        if n.kind == KIND_GROUP {
            for m in n.reference.members.iter().flatten() {
                if !ids.contains(m.as_str()) {
                    f.push(Finding::refuse(
                        "group-member",
                        Some(n.id.clone()),
                        format!("member {m:?} is not a node in this document"),
                    ));
                }
            }
        }
    }

    // --- edges --------------------------------------------------------
    for (i, e) in doc.edges.iter().enumerate() {
        let at = Some(format!("edge[{i}] {}→{}", e.from, e.to));
        if !is_valid_edge_kind(&e.kind) {
            f.push(Finding::refuse(
                "edge-kind",
                at.clone(),
                format!(
                    "unknown edge kind {:?} — the vocabulary is {}",
                    e.kind,
                    EDGE_KINDS.join(", ")
                ),
            ));
        }
        for (side, id) in [("from", &e.from), ("to", &e.to)] {
            if !ids.contains(id.as_str()) {
                f.push(Finding::refuse(
                    "edge-endpoint",
                    at.clone(),
                    format!("{side} {id:?} is not a node in this document"),
                ));
            }
        }
        if e.from == e.to {
            f.push(Finding::refuse(
                "edge-endpoint",
                at.clone(),
                "an edge may not point a node at itself",
            ));
        }
        match e.provenance_or_default() {
            PROVENANCE_AUTHORED => {
                if e.trust.is_some() {
                    f.push(Finding::refuse(
                        "edge-provenance",
                        at.clone(),
                        "an AUTHORED edge carries no trust class — a human's arrow is not \
                         a claim the index can back (D10: derived edges carry their class, \
                         authored edges one stroke). Drop `trust`, or set \
                         `provenance: \"derived\"` if this edge came from a kb-code lane",
                    ));
                }
            }
            PROVENANCE_DERIVED => match e.trust.as_deref() {
                None => f.push(Finding::refuse(
                    "edge-provenance",
                    at.clone(),
                    format!(
                        "a DERIVED edge must name the trust class it inherited — one of {}",
                        EDGE_TRUST.join(", ")
                    ),
                )),
                Some(t) if !EDGE_TRUST.contains(&t) => f.push(Finding::refuse(
                    "edge-provenance",
                    at.clone(),
                    format!(
                        "unknown trust class {t:?} — the vocabulary is {}",
                        EDGE_TRUST.join(", ")
                    ),
                )),
                _ => {}
            },
            other => f.push(Finding::refuse(
                "edge-provenance",
                at.clone(),
                format!(
                    "unknown provenance {other:?} — expected {PROVENANCE_AUTHORED} or \
                     {PROVENANCE_DERIVED}"
                ),
            )),
        }
    }

    // --- steps --------------------------------------------------------
    let mut stepped: BTreeSet<&str> = BTreeSet::new();
    for (i, s) in doc.steps.iter().enumerate() {
        let at = Some(format!("step[{i}] {}", s.node));
        if !ids.contains(s.node.as_str()) {
            f.push(Finding::refuse(
                "step-node",
                at.clone(),
                format!(
                    "step names {:?}, which is not a node in this document",
                    s.node
                ),
            ));
            continue;
        }
        if !stepped.insert(s.node.as_str()) {
            f.push(Finding::refuse(
                "step-node",
                at,
                "a node appears twice in the walkthrough order — a step order is a \
                 sequence of distinct stops, not a loop",
            ));
        }
    }
    if doc.nodes.len() > STEPS_REQUIRED_ABOVE && doc.steps.is_empty() {
        f.push(Finding::refuse(
            "steps-required",
            None,
            format!(
                "{} nodes with no `steps` — above {STEPS_REQUIRED_ABOVE} a board must \
                 declare a reading order, because nothing else in the document says \
                 where to start",
                doc.nodes.len()
            ),
        ));
    }

    // --- pins ---------------------------------------------------------
    for (node, pin) in &doc.pins {
        if !ids.contains(node.as_str()) {
            f.push(Finding::refuse(
                "pin-node",
                Some(node.clone()),
                "a pin names a node that is not in this document",
            ));
        }
        if !pin.x.is_finite() || !pin.y.is_finite() {
            f.push(Finding::refuse(
                "pin-node",
                Some(node.clone()),
                "a pin's x and y must be finite numbers",
            ));
        }
    }

    // --- connectivity --------------------------------------------------
    let components = components(doc);
    if components.len() > 1 {
        let names: Vec<String> = components
            .iter()
            .map(|c| format!("[{}]", c.join(", ")))
            .collect();
        if components.len() > MAX_COMPONENTS && !opts.allow_disconnected {
            f.push(Finding::refuse(
                "components",
                None,
                format!(
                    "{} disconnected components (cap is {MAX_COMPONENTS}): {}. A board with \
                     more islands than that is a list, not a map — connect them, group \
                     them, or pass allow_disconnected",
                    components.len(),
                    names.join(" ")
                ),
            ));
        } else {
            f.push(Finding::warn(
                "components",
                None,
                format!(
                    "{} disconnected components: {}",
                    components.len(),
                    names.join(" ")
                ),
            ));
        }
    }

    Report {
        findings: f,
        components,
    }
}

/// Per-kind reference validation: the fields this kind REQUIRES, and the
/// fields that belong to some other kind and must not be here.
fn check_reference(n: &NodeIn, f: &mut Vec<Finding>) {
    let r = &n.reference;
    let at = || Some(n.id.clone());
    // (field name, is-present) for every field, so "stray field" is a
    // subtraction rather than ten hand-written match arms that can drift.
    let present: Vec<(&str, bool)> = vec![
        ("path", r.path.is_some()),
        ("symbol", r.symbol.is_some()),
        ("range", r.range.is_some()),
        ("context", r.context.is_some()),
        ("blob_sha", r.blob_sha.is_some()),
        ("guard_hash", r.guard_hash.is_some()),
        ("query", r.query.is_some()),
        ("authored_count", r.authored_count.is_some()),
        ("review", r.review.is_some()),
        ("patchset", r.patchset.is_some()),
        ("hunk", r.hunk.is_some()),
        ("finding", r.finding.is_some()),
        ("annotation", r.annotation.is_some()),
        ("session", r.session.is_some()),
        ("turn", r.turn.is_some()),
        ("bookmark", r.bookmark.is_some()),
        ("members", r.members.is_some()),
        ("url", r.url.is_some()),
    ];
    let (required, optional): (&[&str], &[&str]) = match n.kind.as_str() {
        KIND_CODE => (
            &["path", "range"],
            &["symbol", "context", "blob_sha", "guard_hash"],
        ),
        KIND_NOTE => (&[], &[]),
        KIND_QUERY => (&["query"], &["authored_count"]),
        KIND_HUNK => (&["review", "patchset", "hunk"], &["path"]),
        KIND_FINDING => (&["review", "finding"], &[]),
        KIND_ANNOTATION => (&["annotation"], &[]),
        KIND_TURN => (&["session", "turn"], &[]),
        KIND_BOOKMARK => (&["bookmark"], &[]),
        KIND_GROUP => (&[], &["members"]),
        KIND_LINK => (&["url"], &[]),
        // Unreachable: `check` returns before calling this on an unknown
        // kind. Present so this match is total without an `unwrap`.
        _ => (&[], &[]),
    };
    for req in required {
        if !present.iter().any(|(k, p)| k == req && *p) {
            f.push(Finding::refuse(
                "node-ref",
                at(),
                format!("a {:?} node requires {req:?}", n.kind),
            ));
        }
    }
    for (field, is_present) in &present {
        if *is_present && !required.contains(field) && !optional.contains(field) {
            f.push(Finding::refuse(
                "node-ref",
                at(),
                format!(
                    "{field:?} does not belong on a {:?} node (it is another kind's \
                     reference field)",
                    n.kind
                ),
            ));
        }
    }

    // Shape checks on the values themselves — MALFORMED is a refusal;
    // "points at something that no longer exists" is `resolve`'s warning.
    if n.kind == KIND_CODE {
        if let Some(p) = &r.path {
            if crate::routes::safe_rel_path(p).is_err() {
                f.push(Finding::refuse(
                    "node-ref",
                    at(),
                    format!("path {p:?} is not a repo-relative path"),
                ));
            }
        }
        for (name, range) in [("range", r.range), ("context", r.context)] {
            if let Some([a, b]) = range {
                if a == 0 || b < a {
                    f.push(Finding::refuse(
                        "node-ref",
                        at(),
                        format!("{name} {a}..{b} must be 1-based with end >= start"),
                    ));
                }
            }
        }
        if let (Some(pr), Some(ctx)) = (r.range, r.context) {
            if ctx[0] > pr[0] || ctx[1] < pr[1] {
                f.push(Finding::refuse(
                    "node-ref",
                    at(),
                    format!(
                        "context {}..{} must CONTAIN the primary range {}..{} — it is what \
                         ±N expands to, not a second range",
                        ctx[0], ctx[1], pr[0], pr[1]
                    ),
                ));
            }
        }
    }
    if n.kind == KIND_HUNK {
        if let Some(h) = &r.hunk {
            if !crate::reviews::is_hunk_id(h) {
                f.push(Finding::refuse(
                    "node-ref",
                    at(),
                    format!(
                        "hunk {h:?} is not a kbc-hunkid/1 address (1-64 lowercase \
                         alphanumerics)"
                    ),
                ));
            }
        }
        if let Some(ps) = r.patchset {
            if ps == 0 {
                f.push(Finding::refuse(
                    "node-ref",
                    at(),
                    "patchset numbers start at 1",
                ));
            }
        }
    }
    if n.kind == KIND_FINDING {
        if let Some(s) = &r.finding {
            if !s.starts_with("f-") || s.len() < 3 || s.len() > MAX_ID_LEN {
                f.push(Finding::refuse(
                    "node-ref",
                    at(),
                    format!("finding {s:?} is not an `f-*` slug"),
                ));
            }
        }
    }
    if n.kind == KIND_ANNOTATION {
        if let Some(a) = &r.annotation {
            if !a.starts_with("ann_") || a.len() > MAX_ID_LEN {
                f.push(Finding::refuse(
                    "node-ref",
                    at(),
                    format!("annotation {a:?} is not an `ann_*` id"),
                ));
            }
        }
    }
    if n.kind == KIND_LINK {
        if let Some(u) = &r.url {
            if !(u.starts_with("https://") || u.starts_with("http://")) {
                f.push(Finding::refuse(
                    "node-ref",
                    at(),
                    format!(
                        "url {u:?} must be http(s) — a `link` node is inert, and a \
                         `javascript:`/`data:` URL in an export would be a script the \
                         export contract forbids"
                    ),
                ));
            }
        }
    }
    if n.kind == KIND_TURN {
        if let Some(t) = &r.turn {
            let body = t.strip_prefix("t-").unwrap_or("");
            if body.is_empty() || !body.chars().all(|c| c.is_ascii_alphanumeric()) {
                f.push(Finding::refuse(
                    "node-ref",
                    at(),
                    format!("turn {t:?} is not a `t-<uuid12>` id"),
                ));
            }
        }
    }
}

/// Weakly-connected components over the node graph, listed by node id in
/// first-appearance order (both of components and of members) — a
/// deterministic answer, because the report is compared in tests and read
/// by an agent.
///
/// GROUP MEMBERSHIP COUNTS AS CONNECTIVITY, in both directions: a board
/// whose cards are gathered into named groups is not a pile of islands
/// just because the author drew no arrows between them.
pub fn components(doc: &BoardDoc) -> Vec<Vec<String>> {
    let order: Vec<&str> = doc.nodes.iter().map(|n| n.id.as_str()).collect();
    let index: BTreeMap<&str, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); order.len()];
    let link = |a: Option<&usize>, b: Option<&usize>, adj: &mut Vec<Vec<usize>>| {
        if let (Some(&a), Some(&b)) = (a, b) {
            if a != b {
                adj[a].push(b);
                adj[b].push(a);
            }
        }
    };
    for e in &doc.edges {
        link(
            index.get(e.from.as_str()),
            index.get(e.to.as_str()),
            &mut adj,
        );
    }
    for n in &doc.nodes {
        if let Some(g) = &n.group {
            link(index.get(n.id.as_str()), index.get(g.as_str()), &mut adj);
        }
        for m in n.reference.members.iter().flatten() {
            link(index.get(n.id.as_str()), index.get(m.as_str()), &mut adj);
        }
    }
    let mut seen = vec![false; order.len()];
    let mut out: Vec<Vec<String>> = Vec::new();
    for start in 0..order.len() {
        if seen[start] {
            continue;
        }
        let mut stack = vec![start];
        seen[start] = true;
        let mut comp: Vec<usize> = Vec::new();
        while let Some(i) = stack.pop() {
            comp.push(i);
            for &j in &adj[i] {
                if !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        // First-appearance order WITHIN a component too — the DFS pop
        // order is an implementation detail and would change under an
        // unrelated edit.
        comp.sort_unstable();
        out.push(comp.into_iter().map(|i| order[i].to_string()).collect());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> BoardDoc {
        BoardDoc {
            schema: SCHEMA.to_string(),
            repo: "r".into(),
            slug: "b".into(),
            title: "T".into(),
            description_md: String::new(),
            status: None,
            authored_ref: None,
            nodes: Vec::new(),
            edges: Vec::new(),
            steps: Vec::new(),
            pins: Default::default(),
        }
    }

    fn node(id: &str, kind: &str) -> NodeIn {
        NodeIn {
            id: id.into(),
            kind: kind.into(),
            title: None,
            body_md: None,
            group: None,
            thread_id: None,
            reference: RefFields::default(),
        }
    }

    fn code_node(id: &str) -> NodeIn {
        let mut n = node(id, KIND_CODE);
        n.reference.path = Some("app/models/order.rb".into());
        n.reference.range = Some([10, 20]);
        n
    }

    fn rules(r: &Report) -> Vec<&'static str> {
        r.findings.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn a_minimal_valid_board_passes_clean() {
        let mut d = base();
        d.nodes.push(code_node("n1"));
        let r = check(&d, Opts::default());
        assert!(!r.refused(), "{:?}", r.findings);
        assert_eq!(r.components, vec![vec!["n1".to_string()]]);
    }

    #[test]
    fn the_coordinate_refusal_fires_on_the_raw_json_and_names_pins() {
        let raw: serde_json::Value = serde_json::from_str(
            r#"{"schema":"kbc-canvas/1","repo":"r","slug":"b","title":"T",
                "nodes":[{"id":"n1","kind":"note","x":10,"y":20}]}"#,
        )
        .unwrap();
        let f = precheck_raw(&raw);
        assert_eq!(f.len(), 2, "{f:?}");
        assert!(f.iter().all(|x| x.rule == "coordinates"));
        assert!(f.iter().all(|x| x.severity == Severity::Refuse));
        assert!(
            f[0].message.contains("pins"),
            "the refusal must teach the sanctioned alternative: {}",
            f[0].message
        );
        assert_eq!(f[0].at.as_deref(), Some("nodes[0].x"));
    }

    #[test]
    fn the_pins_map_is_the_one_place_x_and_y_are_allowed() {
        let raw: serde_json::Value = serde_json::from_str(
            r#"{"schema":"kbc-canvas/1","repo":"r","slug":"b","title":"T",
                "nodes":[{"id":"n1","kind":"note"}],
                "pins":{"n1":{"x":10,"y":20}}}"#,
        )
        .unwrap();
        assert!(precheck_raw(&raw).is_empty());
    }

    #[test]
    fn a_deeply_nested_coordinate_is_still_found() {
        let raw: serde_json::Value =
            serde_json::from_str(r#"{"nodes":[{"layout":{"position":{"a":1}}}]}"#).unwrap();
        let f = precheck_raw(&raw);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].at.as_deref(), Some("nodes[0].layout.position"));
    }

    #[test]
    fn the_node_cap_refuses_with_the_count() {
        let mut d = base();
        for i in 0..=MAX_NODES {
            d.nodes.push(node(&format!("n{i}"), KIND_NOTE));
        }
        d.steps = d
            .nodes
            .iter()
            .map(|n| StepIn {
                node: n.id.clone(),
                caption: None,
            })
            .collect();
        let r = check(&d, Opts::default());
        assert!(r.refused());
        let m = &r
            .refusals()
            .find(|f| f.rule == "node-cap")
            .expect("node-cap")
            .message;
        assert!(m.contains(&(MAX_NODES + 1).to_string()), "{m}");
        assert!(m.contains(&MAX_NODES.to_string()), "{m}");
    }

    #[test]
    fn steps_are_required_above_the_threshold_and_not_below() {
        let mut d = base();
        for i in 0..=STEPS_REQUIRED_ABOVE {
            d.nodes.push(node(&format!("n{i}"), KIND_NOTE));
        }
        d.pins.clear();
        let r = check(
            &d,
            Opts {
                allow_disconnected: true,
            },
        );
        assert!(rules(&r).contains(&"steps-required"), "{:?}", r.findings);
        d.nodes.pop();
        let r = check(
            &d,
            Opts {
                allow_disconnected: true,
            },
        );
        assert!(!rules(&r).contains(&"steps-required"), "{:?}", r.findings);
    }

    #[test]
    fn disconnection_warns_under_the_cap_and_refuses_over_it_unless_allowed() {
        let mut d = base();
        for i in 0..MAX_COMPONENTS {
            d.nodes.push(node(&format!("n{i}"), KIND_NOTE));
        }
        let r = check(&d, Opts::default());
        assert!(!r.refused(), "{:?}", r.findings);
        assert_eq!(r.warnings().filter(|f| f.rule == "components").count(), 1);
        assert_eq!(r.components.len(), MAX_COMPONENTS);

        d.nodes.push(node("nx", KIND_NOTE));
        let r = check(&d, Opts::default());
        assert!(r.refused());
        assert!(rules(&r).contains(&"components"));
        let r = check(
            &d,
            Opts {
                allow_disconnected: true,
            },
        );
        assert!(!r.refused(), "{:?}", r.findings);
    }

    #[test]
    fn group_membership_counts_as_connectivity() {
        let mut d = base();
        let mut g = node("g1", KIND_GROUP);
        g.reference.members = Some(vec!["n1".into(), "n2".into()]);
        d.nodes.push(g);
        let mut a = node("n1", KIND_NOTE);
        a.group = Some("g1".into());
        let mut b = node("n2", KIND_NOTE);
        b.group = Some("g1".into());
        d.nodes.push(a);
        d.nodes.push(b);
        let r = check(&d, Opts::default());
        assert!(!r.refused(), "{:?}", r.findings);
        assert_eq!(r.components.len(), 1, "{:?}", r.components);
        assert_eq!(r.components[0], vec!["g1", "n1", "n2"]);
    }

    #[test]
    fn an_authored_edge_may_not_carry_trust_and_a_derived_one_must() {
        let mut d = base();
        d.nodes.push(node("a", KIND_NOTE));
        d.nodes.push(node("b", KIND_NOTE));
        d.edges.push(EdgeIn {
            from: "a".into(),
            to: "b".into(),
            kind: "calls".into(),
            label: None,
            provenance: None,
            trust: Some("exact".into()),
        });
        let r = check(&d, Opts::default());
        assert!(rules(&r).contains(&"edge-provenance"), "{:?}", r.findings);

        d.edges[0].trust = None;
        assert!(!check(&d, Opts::default()).refused());

        d.edges[0].provenance = Some(PROVENANCE_DERIVED.into());
        let r = check(&d, Opts::default());
        assert!(rules(&r).contains(&"edge-provenance"));
        d.edges[0].trust = Some("likely".into());
        assert!(!check(&d, Opts::default()).refused());
        d.edges[0].trust = Some("probable".into());
        assert!(check(&d, Opts::default()).refused());
    }

    #[test]
    fn an_edge_endpoint_or_step_naming_an_absent_node_is_refused() {
        let mut d = base();
        d.nodes.push(node("a", KIND_NOTE));
        d.edges.push(EdgeIn {
            from: "a".into(),
            to: "ghost".into(),
            kind: "then".into(),
            label: None,
            provenance: None,
            trust: None,
        });
        d.steps.push(StepIn {
            node: "ghost".into(),
            caption: None,
        });
        let r = check(&d, Opts::default());
        assert!(rules(&r).contains(&"edge-endpoint"));
        assert!(rules(&r).contains(&"step-node"));
    }

    #[test]
    fn a_reference_field_from_another_kind_is_refused_by_name() {
        let mut d = base();
        let mut n = node("n1", KIND_NOTE);
        n.reference.path = Some("a.rb".into());
        d.nodes.push(n);
        let r = check(&d, Opts::default());
        let m = &r.refusals().find(|f| f.rule == "node-ref").unwrap().message;
        assert!(m.contains("path"), "{m}");
        assert!(m.contains("note"), "{m}");
    }

    #[test]
    fn every_kind_has_a_required_field_set_that_a_bare_node_fails_or_passes() {
        // Exhaustive over the vocabulary: adding a kind without teaching
        // `check_reference` about it fails HERE.
        let bare_ok = [KIND_NOTE, KIND_GROUP];
        for k in NODE_KINDS {
            let mut d = base();
            d.nodes.push(node("n1", k));
            let refused = check(&d, Opts::default()).refused();
            assert_eq!(
                refused,
                !bare_ok.contains(&k),
                "kind {k:?}: a bare node with no reference fields should {} be refused",
                if bare_ok.contains(&k) { "NOT" } else { "" }
            );
        }
    }

    #[test]
    fn a_malformed_address_is_a_refusal_shape_by_shape() {
        // One malformed-address case: the kind, and a mutation that
        // writes the bad reference onto a bare node of that kind.
        type Case = (&'static str, Box<dyn Fn(&mut NodeIn)>);
        let cases: Vec<Case> = vec![
            (
                KIND_CODE,
                Box::new(|n: &mut NodeIn| {
                    n.reference.path = Some("../etc/passwd".into());
                    n.reference.range = Some([1, 2]);
                }),
            ),
            (
                KIND_CODE,
                Box::new(|n: &mut NodeIn| {
                    n.reference.path = Some("a.rb".into());
                    n.reference.range = Some([0, 2]);
                }),
            ),
            (
                KIND_CODE,
                Box::new(|n: &mut NodeIn| {
                    n.reference.path = Some("a.rb".into());
                    n.reference.range = Some([5, 9]);
                    n.reference.context = Some([6, 9]);
                }),
            ),
            (
                KIND_HUNK,
                Box::new(|n: &mut NodeIn| {
                    n.reference.review = Some(1);
                    n.reference.patchset = Some(1);
                    n.reference.hunk = Some("NOT A HUNK".into());
                }),
            ),
            (
                KIND_FINDING,
                Box::new(|n: &mut NodeIn| {
                    n.reference.review = Some(1);
                    n.reference.finding = Some("race".into());
                }),
            ),
            (
                KIND_ANNOTATION,
                Box::new(|n: &mut NodeIn| n.reference.annotation = Some("nope".into())),
            ),
            (
                KIND_LINK,
                Box::new(|n: &mut NodeIn| n.reference.url = Some("javascript:alert(1)".into())),
            ),
            (
                KIND_TURN,
                Box::new(|n: &mut NodeIn| {
                    n.reference.session = Some("s".into());
                    n.reference.turn = Some("turn-1".into());
                }),
            ),
        ];
        for (kind, mutate) in cases {
            let mut d = base();
            let mut n = node("n1", kind);
            mutate(&mut n);
            d.nodes.push(n);
            let r = check(&d, Opts::default());
            assert!(
                r.refused(),
                "a malformed {kind} address must be refused: {:?}",
                r.findings
            );
        }
    }

    #[test]
    fn apply_may_not_write_an_accepted_status() {
        let mut d = base();
        d.nodes.push(node("n1", KIND_NOTE));
        for s in [STATUS_ACCEPTED, STATUS_ARCHIVED, "nonsense"] {
            d.status = Some(s.into());
            let r = check(&d, Opts::default());
            assert!(rules(&r).contains(&"status"), "{s} must be refused");
        }
        for s in APPLIABLE_STATUSES {
            d.status = Some(s.into());
            assert!(!check(&d, Opts::default()).refused(), "{s} must be allowed");
        }
    }

    #[test]
    fn the_refusal_summary_names_every_rule_and_its_count() {
        let mut d = base();
        d.schema = "canvas/1".into();
        d.slug = "Bad Slug".into();
        let r = check(&d, Opts::default());
        let s = r.refusal_summary();
        assert!(s.contains("2 lint rule"), "{s}");
        assert!(s.contains("[schema]"), "{s}");
        assert!(s.contains("[slug]"), "{s}");
    }

    #[test]
    fn a_pin_for_an_absent_node_is_refused() {
        let mut d = base();
        d.nodes.push(node("n1", KIND_NOTE));
        d.pins.insert("ghost".into(), Pin { x: 1.0, y: 1.0 });
        assert!(rules(&check(&d, Opts::default())).contains(&"pin-node"));
    }
}
