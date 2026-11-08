//! kb-session-work/1 — a pure, LLM-free, unscored grouping of sessions into
//! pieces of work by a FIXED ladder of named terms. Surfaced, never scored
//! (#11): no module under `sessions/` that ranks may name this module.
//!
//! The ladder is the wire contract: [`TERMS`] lists the five grouping
//! terms in the order edges are applied, and a group's `term` is the first
//! term that contributed an edge (`"alone"` when none did). There is no
//! score, rank, weight, confidence or relevance anywhere in this module:
//! groups are ordered by recency only (last activity, newest first).
//!
//! Inputs are the per-capture rows the route scans (newest capture per
//! session), plus slate-takeover edges derived from slate structure only.
//! [`group_work`] is deterministic and independent of input order.

use std::collections::{BTreeMap, HashMap, HashSet};

/// The fixed ladder: term names in the order edges are applied. `term` of
/// a group is the first of these that contributed at least one edge.
pub const TERMS: [&str; 5] = ["tree", "segment", "job", "slate_take", "cwd_time"];

/// Weak `cwd_time` links only join two singletons whose start follows the
/// previous one's end by at most this many seconds (inclusive).
pub const WORK_CWD_GAP_SECS: i64 = 1800;

/// A job ulid carried by more than this many logical sessions is not
/// trusted as a grouping signal: it adds no edges and is reported as a
/// skipped term instead. Exactly this many still joins.
pub const WORK_JOB_FANOUT_CAP: usize = 50;

/// Members surfaced per group; the full count still rides as
/// `members_total`.
pub const WORK_MEMBERS_CAP: usize = 25;

/// One scanned (newest-capture) session row, normalised for grouping.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkSession {
    pub kb: String,
    pub session_id: String,
    pub segment_of: Option<String>,
    pub root_session_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub started_at: i64,
    pub ended_at: i64,
    pub active_secs: i64,
    pub harness: String,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub job_ulids: Vec<String>,
}

/// One slate-takeover edge: session `from_session` handed its work to
/// session `to_session` (a `take` post replying to a `take`/`hand` post).
#[derive(Debug, Clone, PartialEq)]
pub struct TakeEdge {
    pub slug: String,
    pub from_session: String,
    pub to_session: String,
}

/// One logical session inside a group: the row set of one
/// `(kb, segment_of or session_id)` node, surfaced as its earliest row.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkMember {
    pub kb: String,
    pub session_id: String,
    pub role: &'static str,
    pub via: &'static str,
    pub started_at: i64,
    pub ended_at: i64,
    pub harness: String,
    pub title: Option<String>,
}

/// The shape counts a group carries. `active_secs` sums non-child members
/// only (a parent's capture already includes its subagents' time, so
/// summing children would double-count).
#[derive(Debug, Clone, PartialEq)]
pub struct WorkCounts {
    pub logical_sessions: u32,
    pub subagents: u32,
    pub parts: u32,
    pub active_secs: i64,
}

/// One piece of work: a connected component of logical sessions joined by
/// the ladder's terms.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkGroup {
    /// `"{kb}:{logical_id}"` of the earliest member. Not a permalink: a
    /// wider scan window can change it.
    pub key: String,
    /// The first ladder term that contributed an edge, or `"alone"`.
    pub term: &'static str,
    /// Every ladder term that contributed at least one edge, ladder order.
    pub terms: Vec<&'static str>,
    pub members: Vec<WorkMember>,
    pub members_total: u32,
    pub counts: WorkCounts,
    pub started_at: i64,
    pub last_activity: i64,
}

/// A ladder term that was applied but skipped, with why.
#[derive(Debug, Clone, PartialEq)]
pub struct SkippedTerm {
    pub term: String,
    pub reason: String,
    pub detail: String,
}

/// The grouping output: recency-ordered groups plus any skipped terms.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkOutcome {
    pub groups: Vec<WorkGroup>,
    pub skipped: Vec<SkippedTerm>,
}

/// One logical session (union-find node): every row sharing
/// `(kb, segment_of or session_id)`.
#[derive(Debug, Clone)]
struct Node {
    kb: String,
    /// The logical id: `segment_of` when segmented, else `session_id`.
    id: String,
    rows: Vec<usize>,
    /// Node-level lineage: the earliest row's stamps win (rows agree in
    /// practice; a stale part's stamp must not split the node).
    root_session_id: Option<String>,
    parent_session_id: Option<String>,
    /// `cwd` of the earliest row — the weak `cwd_time` key.
    cwd: Option<String>,
    started_at: i64,
    ended_at: i64,
    active_secs: i64,
}

impl Node {
    fn parts(&self) -> u32 {
        self.rows.len() as u32
    }
    fn is_child(&self) -> bool {
        self.parent_session_id.is_some()
    }
}

/// The representative (earliest) row of a node, by `(started_at, session_id)`.
fn rep_row<'a>(node: &Node, rows: &[&'a WorkSession]) -> Option<&'a WorkSession> {
    node.rows
        .iter()
        .map(|&i| rows[i])
        .min_by(|a, b| (a.started_at, &a.session_id).cmp(&(b.started_at, &b.session_id)))
}

/// Does node `i` carry a job ulid on any of its rows?
fn node_carries_job(i: usize, nodes: &[Node], rows: &[&WorkSession]) -> bool {
    nodes[i]
        .rows
        .iter()
        .any(|&ri| !rows[ri].job_ulids.is_empty())
}

/// A cwd_time candidate: no lineage stamps and no job ulid, so no strong
/// edge can pull it into a group.
fn is_cwd_candidate(i: usize, nodes: &[Node], rows: &[&WorkSession]) -> bool {
    nodes[i].root_session_id.is_none()
        && nodes[i].parent_session_id.is_none()
        && !node_carries_job(i, nodes, rows)
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        let next = parent[x];
        parent[x] = parent[next];
        x = next;
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        // Keep the smaller index as root for determinism.
        if ra < rb {
            parent[rb] = ra;
        } else {
            parent[ra] = rb;
        }
    }
}

fn node_order(nodes: &[Node], a: usize, b: usize) -> std::cmp::Ordering {
    (nodes[a].started_at, &nodes[a].kb, &nodes[a].id).cmp(&(
        nodes[b].started_at,
        &nodes[b].kb,
        &nodes[b].id,
    ))
}

/// Group `sessions` into pieces of work. Deterministic: inputs are sorted
/// by `(started_at, kb, session_id)` first, so the output is identical for
/// any permutation of the inputs.
pub fn group_work(sessions: &[WorkSession], takes: &[TakeEdge]) -> WorkOutcome {
    // Canonical order first: every later iteration is then input-order
    // independent.
    let mut rows: Vec<&WorkSession> = sessions.iter().collect();
    rows.sort_by(|a, b| {
        (a.started_at, &a.kb, &a.session_id).cmp(&(b.started_at, &b.kb, &b.session_id))
    });

    // --- step 1: fold rows into logical-session nodes -------------------
    let mut nodes: Vec<Node> = Vec::new();
    let mut node_index: HashMap<(String, String), usize> = HashMap::new();
    for (idx, r) in rows.iter().enumerate() {
        let logical = r.segment_of.clone().unwrap_or_else(|| r.session_id.clone());
        let key = (r.kb.clone(), logical.clone());
        if let Some(&ni) = node_index.get(&key) {
            let node = &mut nodes[ni];
            node.rows.push(idx);
            node.started_at = node.started_at.min(r.started_at);
            node.ended_at = node.ended_at.max(r.ended_at);
            node.active_secs = node.active_secs.saturating_add(r.active_secs);
            if node.root_session_id.is_none() {
                node.root_session_id = r.root_session_id.clone();
            }
            if node.parent_session_id.is_none() {
                node.parent_session_id = r.parent_session_id.clone();
            }
        } else {
            node_index.insert(key, nodes.len());
            nodes.push(Node {
                kb: r.kb.clone(),
                id: logical,
                rows: vec![idx],
                root_session_id: r.root_session_id.clone(),
                parent_session_id: r.parent_session_id.clone(),
                cwd: r.cwd.clone(),
                started_at: r.started_at,
                ended_at: r.ended_at,
                active_secs: r.active_secs,
            });
        }
    }

    let mut parent: Vec<usize> = (0..nodes.len()).collect();
    let mut skipped: Vec<SkippedTerm> = Vec::new();

    // --- step 2a: tree ---------------------------------------------------
    // Every node whose row carries a root unions to the node
    // `(kb, root)` when that root's capture is in the window. Children of
    // a root outside the window union to each other through the shared
    // root id, so the orphan tree is still one piece of work.
    {
        let mut root_groups: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (i, node) in nodes.iter().enumerate() {
            if let Some(root) = &node.root_session_id {
                root_groups
                    .entry((node.kb.clone(), root.clone()))
                    .or_default()
                    .push(i);
            }
        }
        for (i, node) in nodes.iter().enumerate() {
            if let Some(root) = &node.root_session_id {
                if let Some(&ri) = node_index.get(&(node.kb.clone(), root.clone())) {
                    union(&mut parent, i, ri);
                }
            }
        }
        for (key, members) in root_groups.iter() {
            if !node_index.contains_key(key) && members.len() > 1 {
                let first = members[0];
                for &other in &members[1..] {
                    union(&mut parent, first, other);
                }
            }
        }
    }

    // --- step 2b: job ----------------------------------------------------
    // Nodes sharing a job ulid join. A ulid carried by more than
    // `WORK_JOB_FANOUT_CAP` nodes adds no edges and is skipped instead.
    {
        let mut by_ulid: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, node) in nodes.iter().enumerate() {
            let mut seen: HashSet<&str> = HashSet::new();
            for &ri in &node.rows {
                for ulid in &rows[ri].job_ulids {
                    if seen.insert(ulid.as_str()) {
                        by_ulid.entry(ulid.clone()).or_default().push(i);
                    }
                }
            }
        }
        for (ulid, members) in by_ulid.iter() {
            if members.len() > WORK_JOB_FANOUT_CAP {
                skipped.push(SkippedTerm {
                    term: "job".to_string(),
                    reason: "fanout_cap".to_string(),
                    detail: format!("{ulid} on {} sessions", members.len()),
                });
                continue;
            }
            if members.len() > 1 {
                let first = members[0];
                for &other in &members[1..] {
                    union(&mut parent, first, other);
                }
            }
        }
    }

    // --- step 2c: slate_take ---------------------------------------------
    // An edge joins only when BOTH session ids resolve to a node. Either
    // a row `session_id` or a row `segment_of` resolves, so a takeover
    // onto a chain resolves to its logical node.
    let resolve = resolve_map(&rows, &node_index);
    {
        let mut seen_edges: HashSet<(usize, usize)> = HashSet::new();
        for edge in takes {
            for cand_kb in node_index
                .keys()
                .map(|(kb, _)| kb.clone())
                .collect::<HashSet<_>>()
            {
                let Some(&a) = resolve.get(&(&cand_kb, edge.from_session.as_str())) else {
                    continue;
                };
                let Some(&b) = resolve.get(&(&cand_kb, edge.to_session.as_str())) else {
                    continue;
                };
                if a == b {
                    continue;
                }
                if seen_edges.insert((a.min(b), a.max(b))) {
                    union(&mut parent, a, b);
                }
            }
        }
    }

    // --- step 3: cwd_time (weak, last) -----------------------------------
    // Only between groups that are a single node with no strong edge:
    // same non-empty cwd and a start that follows the previous end by
    // 0..=WORK_CWD_GAP_SECS, chained in time order over adjacent pairs.
    // Snapshot the component sizes now (before any cwd union) so the
    // mutability of `parent` is simple.
    let comp_sizes: HashMap<usize, usize> = {
        let mut comps: HashMap<usize, usize> = HashMap::new();
        for i in 0..nodes.len() {
            *comps.entry(find(&mut parent, i)).or_insert(0) += 1;
        }
        comps
    };
    {
        let mut by_cwd: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for i in 0..nodes.len() {
            let root = find(&mut parent, i);
            if comp_sizes.get(&root).copied().unwrap_or(0) != 1 {
                continue;
            }
            if !is_cwd_candidate(i, &nodes, &rows) {
                continue;
            }
            if let Some(cwd) = nodes[i].cwd.clone() {
                if !cwd.is_empty() {
                    by_cwd.entry(cwd).or_default().push(i);
                }
            }
        }
        for (_cwd, members) in by_cwd.iter() {
            let mut seq: Vec<usize> = members.clone();
            seq.sort_by(|&a, &b| node_order(&nodes, a, b));
            for pair in seq.windows(2) {
                let (prev, next) = (pair[0], pair[1]);
                let gap = nodes[next].started_at - nodes[prev].ended_at;
                if (0..=WORK_CWD_GAP_SECS).contains(&gap) {
                    union(&mut parent, prev, next);
                }
            }
        }
    }

    // --- step 4: build groups --------------------------------------------
    let mut comps: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..nodes.len() {
        comps.entry(find(&mut parent, i)).or_default().push(i);
    }

    let mut groups: Vec<WorkGroup> = Vec::new();
    for (_root, members) in comps.iter() {
        // Which terms contributed an edge inside this component? The same
        // predicates as the strong phases, evaluated per component.
        let terms = component_terms(members, &nodes, &rows, &node_index, &resolve, takes);
        let term = terms.first().copied().unwrap_or("alone");

        let mut members = members.clone();
        members.sort_by(|&a, &b| node_order(&nodes, a, b));
        // Root first: a node whose logical id is one of the component's
        // root ids leads the member list.
        let root_ids: HashSet<(String, String)> = members
            .iter()
            .filter_map(|&i| {
                nodes[i]
                    .root_session_id
                    .clone()
                    .map(|r| (nodes[i].kb.clone(), r))
            })
            .filter(|(kb, root)| {
                node_index
                    .get(&(kb.clone(), root.clone()))
                    .is_some_and(|&ri| members.contains(&ri))
            })
            .collect();
        if let Some(pos) = members
            .iter()
            .position(|&i| root_ids.contains(&(nodes[i].kb.clone(), nodes[i].id.clone())))
        {
            let first = members.remove(pos);
            members.insert(0, first);
        }

        let members_total = members.len() as u32;
        let started_at = members
            .iter()
            .map(|&i| nodes[i].started_at)
            .min()
            .unwrap_or(0);
        let last_activity = members
            .iter()
            .map(|&i| nodes[i].ended_at)
            .max()
            .unwrap_or(0);
        let counts = WorkCounts {
            logical_sessions: members.len() as u32,
            subagents: members.iter().filter(|&&i| nodes[i].is_child()).count() as u32,
            parts: members.iter().map(|&i| nodes[i].parts()).sum(),
            active_secs: members
                .iter()
                .filter(|&&i| !nodes[i].is_child())
                .map(|&i| nodes[i].active_secs)
                .sum(),
        };

        let shown: Vec<WorkMember> = members
            .iter()
            .take(WORK_MEMBERS_CAP)
            .map(|&i| {
                let node = &nodes[i];
                let (role, via) = if node.is_child() {
                    ("child", "tree")
                } else if root_ids.contains(&(node.kb.clone(), node.id.clone())) {
                    ("root", "tree")
                } else {
                    ("member", term)
                };
                let rep = rep_row(node, &rows);
                WorkMember {
                    kb: node.kb.clone(),
                    session_id: rep
                        .map(|r| r.session_id.clone())
                        .unwrap_or_else(|| node.id.clone()),
                    role,
                    via,
                    started_at: node.started_at,
                    ended_at: node.ended_at,
                    harness: rep.map(|r| r.harness.clone()).unwrap_or_default(),
                    title: rep.and_then(|r| r.title.clone()),
                }
            })
            .collect();

        let earliest = members
            .iter()
            .min_by(|&&a, &&b| node_order(&nodes, a, b))
            .map(|&i| &nodes[i])
            .unwrap();
        let key = format!("{}:{}", earliest.kb, earliest.id);

        groups.push(WorkGroup {
            key,
            term,
            terms,
            members: shown,
            members_total,
            counts,
            started_at,
            last_activity,
        });
    }

    // --- step 5: recency order only ---------------------------------------
    groups.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.key.cmp(&b.key))
    });

    WorkOutcome { groups, skipped }
}

/// Map every (kb, session id / segment_of) spelling to its node.
fn resolve_map<'a>(
    rows: &[&'a WorkSession],
    node_index: &HashMap<(String, String), usize>,
) -> HashMap<(&'a str, &'a str), usize> {
    let mut resolve: HashMap<(&str, &str), usize> = HashMap::new();
    for r in rows {
        let ni = node_index[&(
            r.kb.clone(),
            r.segment_of.clone().unwrap_or_else(|| r.session_id.clone()),
        )];
        resolve
            .entry((r.kb.as_str(), r.session_id.as_str()))
            .or_insert(ni);
        if let Some(of) = &r.segment_of {
            resolve.entry((r.kb.as_str(), of.as_str())).or_insert(ni);
        }
    }
    resolve
}

/// The ladder terms that contributed at least one edge inside one
/// component, in ladder order.
fn component_terms(
    members: &[usize],
    nodes: &[Node],
    rows: &[&WorkSession],
    node_index: &HashMap<(String, String), usize>,
    resolve: &HashMap<(&str, &str), usize>,
    takes: &[TakeEdge],
) -> Vec<&'static str> {
    let mut terms: Vec<&'static str> = Vec::new();
    let member_set: HashSet<usize> = members.iter().copied().collect();

    // tree: a member's root node is also a member (or 2+ members share an
    // out-of-window root).
    {
        let mut shared_root: HashMap<(String, String), usize> = HashMap::new();
        let mut tree = false;
        for &i in members {
            if let Some(root) = &nodes[i].root_session_id {
                let key = (nodes[i].kb.clone(), root.clone());
                *shared_root.entry(key.clone()).or_insert(0) += 1;
                if let Some(&ri) = node_index.get(&key) {
                    if member_set.contains(&ri) {
                        tree = true;
                    }
                }
            }
        }
        if tree || shared_root.values().any(|&c| c > 1) {
            terms.push(TERMS[0]);
        }
    }

    // segment: some member node has more than one part.
    if members.iter().any(|&i| nodes[i].parts() > 1) {
        terms.push(TERMS[1]);
    }

    // job: two members share a ulid.
    {
        let mut ulid_nodes: HashMap<String, usize> = HashMap::new();
        let mut job = false;
        for &i in members {
            let mut seen: HashSet<&str> = HashSet::new();
            for &ri in &nodes[i].rows {
                for ulid in &rows[ri].job_ulids {
                    if seen.insert(ulid.as_str()) {
                        if let Some(&prev) = ulid_nodes.get(ulid) {
                            if prev != i {
                                job = true;
                            }
                        } else {
                            ulid_nodes.insert(ulid.clone(), i);
                        }
                    }
                }
            }
        }
        if job {
            terms.push(TERMS[2]);
        }
    }

    // slate_take: a take edge between two members.
    {
        let mut take = false;
        'outer: for edge in takes {
            for cand_kb in node_index
                .keys()
                .map(|(kb, _)| kb.clone())
                .collect::<HashSet<_>>()
            {
                let (Some(&a), Some(&b)) = (
                    resolve.get(&(&cand_kb, edge.from_session.as_str())),
                    resolve.get(&(&cand_kb, edge.to_session.as_str())),
                ) else {
                    continue;
                };
                if a != b && member_set.contains(&a) && member_set.contains(&b) {
                    take = true;
                    break 'outer;
                }
            }
        }
        if take {
            terms.push(TERMS[3]);
        }
    }

    // cwd_time: 2+ candidate members chained weakly (the only way a
    // no-strong-edge component exceeds one node).
    if members.len() > 1 && members.iter().all(|&i| is_cwd_candidate(i, nodes, rows)) {
        let mut seq: Vec<usize> = members.to_vec();
        seq.sort_by(|&a, &b| node_order(nodes, a, b));
        let mut chained = false;
        for pair in seq.windows(2) {
            let (prev, next) = (pair[0], pair[1]);
            if let (Some(pc), Some(nc)) = (&nodes[prev].cwd, &nodes[next].cwd) {
                if !pc.is_empty() && pc == nc {
                    let gap = nodes[next].started_at - nodes[prev].ended_at;
                    if (0..=WORK_CWD_GAP_SECS).contains(&gap) {
                        chained = true;
                        break;
                    }
                }
            }
        }
        if chained {
            terms.push(TERMS[4]);
        }
    }

    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(kb: &str, session_id: &str, started_at: i64, ended_at: i64) -> WorkSession {
        WorkSession {
            kb: kb.to_string(),
            session_id: session_id.to_string(),
            segment_of: None,
            root_session_id: None,
            parent_session_id: None,
            started_at,
            ended_at,
            active_secs: ended_at - started_at,
            harness: "omp".to_string(),
            cwd: Some("/w/x".to_string()),
            title: None,
            job_ulids: Vec::new(),
        }
    }

    #[test]
    fn ladder_order_is_the_wire_contract() {
        assert_eq!(TERMS, ["tree", "segment", "job", "slate_take", "cwd_time"]);
        assert_eq!(WORK_CWD_GAP_SECS, 1800);
        assert_eq!(WORK_JOB_FANOUT_CAP, 50);
        assert_eq!(WORK_MEMBERS_CAP, 25);
    }

    #[test]
    fn tree_groups_children_under_their_root_even_when_the_root_is_outside_the_window() {
        let mut a = ws("smoke", "child-a", 100, 200);
        a.root_session_id = Some("root-x".to_string());
        a.parent_session_id = Some("root-x".to_string());
        let mut b = ws("smoke", "child-b", 150, 250);
        b.root_session_id = Some("root-x".to_string());
        b.parent_session_id = Some("root-x".to_string());
        let out = group_work(&[a, b], &[]);
        assert_eq!(out.groups.len(), 1, "{out:?}");
        let g = &out.groups[0];
        assert_eq!(g.term, "tree");
        assert_eq!(g.terms, vec!["tree"]);
        assert_eq!(g.members_total, 2);
        // The missing root is not a member; the key uses the earliest
        // member (child-a).
        assert_eq!(g.key, "smoke:child-a");
        assert!(g.members.iter().all(|m| m.role == "child"));
    }

    #[test]
    fn tree_group_with_root_present_lists_the_root_first() {
        let root = ws("smoke", "root-x", 50, 500);
        let mut a = ws("smoke", "child-a", 100, 200);
        a.root_session_id = Some("root-x".to_string());
        a.parent_session_id = Some("root-x".to_string());
        let mut b = ws("smoke", "child-b", 150, 250);
        b.root_session_id = Some("root-x".to_string());
        b.parent_session_id = Some("root-x".to_string());
        let out = group_work(&[a, b, root], &[]);
        assert_eq!(out.groups.len(), 1, "{out:?}");
        let g = &out.groups[0];
        assert_eq!(g.key, "smoke:root-x");
        assert_eq!(g.members[0].session_id, "root-x");
        assert_eq!(g.members[0].role, "root");
        assert_eq!(g.members_total, 3);
        assert_eq!(g.counts.logical_sessions, 3);
        assert_eq!(g.counts.subagents, 2);
    }

    #[test]
    fn segment_parts_are_one_logical_session() {
        let mut p1 = ws("smoke", "x-p01", 100, 200);
        p1.segment_of = Some("x".to_string());
        let mut p2 = ws("smoke", "x-p02", 300, 450);
        p2.segment_of = Some("x".to_string());
        let out = group_work(&[p1, p2], &[]);
        assert_eq!(out.groups.len(), 1, "{out:?}");
        let g = &out.groups[0];
        assert_eq!(g.counts.logical_sessions, 1);
        assert_eq!(g.counts.parts, 2);
        assert_eq!(g.counts.active_secs, 250);
        assert!(g.terms.contains(&"segment"), "{g:?}");
    }

    #[test]
    fn job_joins_nodes_sharing_a_ulid_and_skips_a_ulid_over_the_fanout_cap() {
        let mut a = ws("smoke", "a", 100, 200);
        a.job_ulids = vec!["ulid-1".to_string()];
        let mut b = ws("smoke", "b", 300, 400);
        b.job_ulids = vec!["ulid-1".to_string()];
        let out = group_work(&[a, b], &[]);
        assert_eq!(out.groups.len(), 1, "{out:?}");
        assert_eq!(out.groups[0].term, "job");

        // 51 carriers: no edges, one SkippedTerm.
        let mut many = Vec::new();
        for i in 0..51 {
            let mut s = ws("smoke", &format!("s{i}"), 100 + i as i64, 200 + i as i64);
            s.job_ulids = vec!["ulid-big".to_string()];
            many.push(s);
        }
        let out = group_work(&many, &[]);
        assert!(out
            .skipped
            .iter()
            .any(|s| s.term == "job" && s.reason == "fanout_cap"));
        assert!(out.groups.iter().all(|g| g.term != "job"), "{out:?}");

        // Exactly 50 still joins.
        let mut fifty = Vec::new();
        for i in 0..50 {
            let mut s = ws("smoke", &format!("t{i}"), 100 + i as i64, 200 + i as i64);
            s.job_ulids = vec!["ulid-50".to_string()];
            fifty.push(s);
        }
        let out = group_work(&fifty, &[]);
        assert_eq!(out.groups.len(), 1, "{out:?}");
        assert_eq!(out.groups[0].term, "job");
        assert!(out.skipped.is_empty());
    }

    #[test]
    fn slate_take_edge_joins_only_resolvable_distinct_sessions() {
        let a = ws("smoke", "a", 100, 200);
        let b = ws("smoke", "b", 300, 400);
        let takes = vec![
            TakeEdge {
                slug: "s".into(),
                from_session: "a".into(),
                to_session: "b".into(),
            },
            TakeEdge {
                slug: "s".into(),
                from_session: "a".into(),
                to_session: "a".into(),
            },
            TakeEdge {
                slug: "s".into(),
                from_session: "a".into(),
                to_session: "unknown".into(),
            },
        ];
        let out = group_work(&[a, b.clone()], &takes);
        assert_eq!(out.groups.len(), 1, "{out:?}");
        assert_eq!(out.groups[0].term, "slate_take");

        // An edge naming a segment chain's raw id resolves to its node.
        let mut p1 = ws("smoke", "x-p01", 0, 100);
        p1.segment_of = Some("x".to_string());
        let edges = vec![TakeEdge {
            slug: "s".into(),
            from_session: "x".into(),
            to_session: "b".into(),
        }];
        let out = group_work(&[p1, b], &edges);
        assert_eq!(out.groups.len(), 1, "{out:?}");
        assert_eq!(out.groups[0].counts.logical_sessions, 2);
        assert_eq!(out.groups[0].terms, vec!["slate_take"]);
    }

    #[test]
    fn cwd_time_links_only_singletons_within_the_gap() {
        let a = ws("smoke", "a", 1000, 1100);
        let b = ws("smoke", "b", 2900, 3000); // gap exactly 1800
        let out = group_work(&[a.clone(), b.clone()], &[]);
        assert_eq!(out.groups.len(), 1, "{out:?}");
        assert_eq!(out.groups[0].term, "cwd_time");

        let c = ws("smoke", "c", 2901, 3000); // gap 1801
        let out = group_work(&[a.clone(), c.clone()], &[]);
        assert_eq!(out.groups.len(), 2);

        let d = ws("smoke", "d", 1050, 1200); // overlap
        let out = group_work(&[a.clone(), d.clone()], &[]);
        assert_eq!(out.groups.len(), 2);

        let mut e = ws("smoke", "e", 1900, 2000);
        e.cwd = Some("/w/other".to_string());
        let out = group_work(&[a.clone(), e.clone()], &[]);
        assert_eq!(out.groups.len(), 2);

        let mut f = ws("smoke", "f", 1900, 2000);
        f.cwd = Some(String::new());
        let out = group_work(&[a.clone(), f], &[]);
        assert_eq!(out.groups.len(), 2);

        // 60 s after a TREE group: the tree group is not a singleton.
        let root = ws("smoke", "root-x", 1000, 1100);
        let mut child = ws("smoke", "child-a", 1000, 1100);
        child.root_session_id = Some("root-x".to_string());
        child.parent_session_id = Some("root-x".to_string());
        let late = ws("smoke", "late", 1160, 1200);
        let out = group_work(&[root, child, late], &[]);
        assert_eq!(out.groups.len(), 2, "{out:?}");
    }

    #[test]
    fn group_work_is_independent_of_input_order() {
        let mut root = ws("smoke", "root-x", 0, 1000);
        root.title = Some("Root".to_string());
        let mut child = ws("smoke", "child-a", 100, 200);
        child.root_session_id = Some("root-x".to_string());
        child.parent_session_id = Some("root-x".to_string());
        let mut p1 = ws("smoke", "x-p01", 50, 60);
        p1.segment_of = Some("x".to_string());
        let mut p2 = ws("smoke", "x-p02", 70, 80);
        p2.segment_of = Some("x".to_string());
        let mut j1 = ws("smoke", "j1", 200, 210);
        j1.job_ulids = vec!["ulid-1".to_string()];
        let mut j2 = ws("smoke", "j2", 220, 230);
        j2.job_ulids = vec!["ulid-1".to_string()];
        let takes = vec![TakeEdge {
            slug: "s".into(),
            from_session: "j1".into(),
            to_session: "x".into(),
        }];
        let base = vec![root, child, p1, p2, j1, j2];
        let expected = group_work(&base, &takes);
        // Every rotation plus the reverse.
        for shift in 0..base.len() {
            let rotated: Vec<WorkSession> = base
                .iter()
                .cycle()
                .skip(shift)
                .take(base.len())
                .cloned()
                .collect();
            assert_eq!(group_work(&rotated, &takes), expected, "rotation {shift}");
        }
        let mut reversed = base.clone();
        reversed.reverse();
        assert_eq!(group_work(&reversed, &takes), expected, "reverse");
    }

    #[test]
    fn child_active_secs_are_not_summed_into_the_group() {
        let root = ws("smoke", "root-x", 0, 100);
        let mut sessions = vec![root];
        for i in 0..5 {
            let mut c = ws("smoke", &format!("child-{i}"), i, i + 100);
            c.root_session_id = Some("root-x".to_string());
            c.parent_session_id = Some("root-x".to_string());
            c.active_secs = 100;
            sessions.push(c);
        }
        let out = group_work(&sessions, &[]);
        assert_eq!(out.groups.len(), 1);
        assert_eq!(out.groups[0].counts.subagents, 5);
        assert_eq!(out.groups[0].counts.active_secs, 100);
    }

    #[test]
    fn members_are_capped_and_totalled() {
        let root = ws("smoke", "root-x", 0, 9000);
        let mut sessions = vec![root];
        for i in 0..60 {
            let mut c = ws("smoke", &format!("child-{i:02}"), i + 1, i + 2);
            c.root_session_id = Some("root-x".to_string());
            c.parent_session_id = Some("root-x".to_string());
            sessions.push(c);
        }
        let out = group_work(&sessions, &[]);
        assert_eq!(out.groups.len(), 1);
        let g = &out.groups[0];
        assert_eq!(g.members.len(), WORK_MEMBERS_CAP);
        assert_eq!(g.members_total, 61);
        assert_eq!(g.members[0].session_id, "root-x");
    }

    #[test]
    fn groups_are_ordered_by_recency_then_key() {
        let a = ws("smoke", "a", 0, 100);
        let b = ws("smoke", "b", 0, 500);
        let c = ws("smoke", "c", 0, 500);
        let out = group_work(&[a, b, c], &[]);
        // b and c share last_activity 500: key ascending breaks the tie.
        assert_eq!(out.groups[0].key, "smoke:b");
        assert_eq!(out.groups[1].key, "smoke:c");
        assert_eq!(out.groups[2].key, "smoke:a");
    }

    // invariant:11 work grouping is surfaced, never scored
    #[test]
    fn no_ranking_module_reads_session_work() {
        const RANKERS: &[(&str, &str)] = &[
            ("memory.rs", include_str!("../memory.rs")),
            ("fusion.rs", include_str!("../fusion.rs")),
            ("resurface.rs", include_str!("../resurface.rs")),
            ("triage.rs", include_str!("../triage.rs")),
            ("query.rs", include_str!("../query.rs")),
            ("sessions/projects.rs", include_str!("projects.rs")),
        ];
        for (name, src) in RANKERS {
            for needle in ["work::", "WorkGroup", "group_work", "kb-session-work"] {
                assert!(
                    !src.contains(needle),
                    "{name} names `{needle}` -- session work grouping is SURFACED, NEVER SCORED \
                     (the kb-session-work/1 #11 amendment): it may be rendered beside a row, never folded \
                     into a rank, a sort key or a filter"
                );
            }
        }
    }
}
