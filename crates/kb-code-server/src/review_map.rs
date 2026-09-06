//! V3.3-S1 (R7) — review map + deterministic reading order.
//!
//! Composes existing import edges, call sites, author_stats, and the
//! review patchset file list into two read surfaces:
//!
//! - `GET /api/reviews/{id}/map` — change set on a static dependency skeleton
//! - `GET /api/reviews/{id}/reading-order` — bottom-up tour over the import DAG
//!
//! Design law: trust classes propagate never improve; same patchset ⇒
//! byte-identical map/order; SCCs collapsed + flagged (`cycle: true`);
//! `agent_touched` is `true|false|null` (null when attribution is absent);
//! `inputs_missing` when import/call extraction has not landed.

use crate::behavioral::is_session_author;
use crate::numstat::FileChange;
use crate::resolve::CLASS_EXACT;
use crate::reviews::{files_changed, require_review, resolve_ps};
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store::{Store, StoreBlocking};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub const MAP_SCHEMA: &str = "review-map/1";
pub const ORDER_SCHEMA: &str = "review-reading-order/1";

// --- pure helpers (unit-tested) --------------------------------------------

/// Detect a test file for reading-order partition (tests last).
///
/// Matches path segments `tests/`, `test/`, `e2e/` (as a directory component)
/// and basenames `*_test.*`, `*.test.*`, `*.spec.*`.
pub fn is_test_path(path: &str) -> bool {
    let p = path.replace('\\', "/");
    let lower = p.to_ascii_lowercase();
    // Directory components.
    for seg in ["tests/", "test/", "e2e/"] {
        if lower.starts_with(seg) || lower.contains(&format!("/{seg}")) {
            return true;
        }
    }
    // Basename patterns.
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    if let Some((stem, _ext)) = base.rsplit_once('.') {
        if stem.ends_with("_test") || stem.ends_with(".test") || stem.ends_with(".spec") {
            return true;
        }
        // `foo.test.ts` → stem is `foo.test` after one rsplit; also
        // `*.test.*` / `*.spec.*` as multi-dot.
        if base.contains(".test.") || base.contains(".spec.") {
            return true;
        }
    }
    false
}

/// Map git name-status letter → wire status.
pub fn status_wire(git_status: &str) -> &'static str {
    match git_status.chars().next().unwrap_or('M') {
        'A' => "added",
        'D' => "deleted",
        'R' | 'C' => "renamed",
        _ => "modified",
    }
}

/// One stop in the reading-order tour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadingStop {
    pub path: String,
    pub reason: String,
    pub cycle: bool,
}

/// Deterministic reading order over a change set.
///
/// 1. Non-test, non-deleted: SCC-collapsed topo over import edges
///    (dependencies first / bottom-up), ties path-asc; cycles flagged.
/// 2. Deleted non-test: path-asc.
/// 3. Tests (any status): path-asc.
///
/// `import_edges` are `(from, to)` where `from` imports `to` (from depends
/// on to). Only edges whose both ends are in the non-test live set matter.
pub fn compute_reading_order(
    files: &[(String, String)], // (path, status_wire)
    import_edges: &[(String, String)],
) -> Vec<ReadingStop> {
    let mut test_paths: Vec<String> = Vec::new();
    let mut deleted_live: Vec<String> = Vec::new(); // non-test deleted
    let mut live: Vec<String> = Vec::new(); // non-test non-deleted

    for (path, status) in files {
        if is_test_path(path) {
            test_paths.push(path.clone());
        } else if status == "deleted" {
            deleted_live.push(path.clone());
        } else {
            live.push(path.clone());
        }
    }
    test_paths.sort();
    deleted_live.sort();
    live.sort();

    // Importer counts among the full changed set (for reasons).
    let changed: HashSet<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    let mut imported_by: HashMap<&str, usize> = HashMap::new();
    for (from, to) in import_edges {
        if changed.contains(from.as_str()) && changed.contains(to.as_str()) {
            *imported_by.entry(to.as_str()).or_default() += 1;
        }
    }

    let reason_for = |path: &str, status_deleted: bool, is_test: bool| -> String {
        if is_test {
            return "test".into();
        }
        if status_deleted {
            return "deleted".into();
        }
        match imported_by.get(path).copied().unwrap_or(0) {
            0 => "no dependency signal".into(),
            n => format!("imported by {n} changed files"),
        }
    };

    // Restrict edges to live non-test set.
    let live_set: HashSet<&str> = live.iter().map(|s| s.as_str()).collect();
    let edges: Vec<(&str, &str)> = import_edges
        .iter()
        .filter(|(f, t)| live_set.contains(f.as_str()) && live_set.contains(t.as_str()))
        .map(|(f, t)| (f.as_str(), t.as_str()))
        .collect();

    let ordered_live = topo_sccs_bottom_up(&live, &edges);

    let mut stops = Vec::with_capacity(files.len());
    for (path, cycle) in ordered_live {
        stops.push(ReadingStop {
            reason: reason_for(&path, false, false),
            path,
            cycle,
        });
    }
    for path in deleted_live {
        stops.push(ReadingStop {
            reason: reason_for(&path, true, false),
            path,
            cycle: false,
        });
    }
    for path in test_paths {
        stops.push(ReadingStop {
            reason: reason_for(&path, false, true),
            path,
            cycle: false,
        });
    }
    stops
}

/// Bottom-up order over the import DAG with SCCs collapsed.
///
/// Edge `(from, to)` means `from` imports `to` (depends on). Dependencies
/// first: process SCCs with no remaining outbound depends-on edges first.
/// Within an SCC: path-asc. `cycle = true` when the SCC has more than one
/// member or a self-loop.
fn topo_sccs_bottom_up(nodes: &[String], edges: &[(&str, &str)]) -> Vec<(String, bool)> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let idx: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i))
        .collect();
    let n = nodes.len();
    // Adjacency: from → to (depends on)
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut radj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut self_loop = vec![false; n];
    for &(f, t) in edges {
        let Some(&fi) = idx.get(f) else { continue };
        let Some(&ti) = idx.get(t) else { continue };
        if fi == ti {
            self_loop[fi] = true;
            continue;
        }
        adj[fi].push(ti);
        radj[ti].push(fi);
    }
    for v in &mut adj {
        v.sort_unstable();
        v.dedup();
    }
    for v in &mut radj {
        v.sort_unstable();
        v.dedup();
    }

    // Kosaraju
    let mut visited = vec![false; n];
    let mut order = Vec::with_capacity(n);
    fn dfs1(u: usize, adj: &[Vec<usize>], visited: &mut [bool], order: &mut Vec<usize>) {
        visited[u] = true;
        for &v in &adj[u] {
            if !visited[v] {
                dfs1(v, adj, visited, order);
            }
        }
        order.push(u);
    }
    for u in 0..n {
        if !visited[u] {
            dfs1(u, &adj, &mut visited, &mut order);
        }
    }
    let mut scc_of = vec![usize::MAX; n];
    let mut sccs: Vec<Vec<usize>> = Vec::new();
    fn dfs2(
        u: usize,
        radj: &[Vec<usize>],
        scc_of: &mut [usize],
        comp: usize,
        members: &mut Vec<usize>,
    ) {
        scc_of[u] = comp;
        members.push(u);
        for &v in &radj[u] {
            if scc_of[v] == usize::MAX {
                dfs2(v, radj, scc_of, comp, members);
            }
        }
    }
    for &u in order.iter().rev() {
        if scc_of[u] == usize::MAX {
            let mut members = Vec::new();
            dfs2(u, &radj, &mut scc_of, sccs.len(), &mut members);
            // Path-asc within SCC (nodes is already path-sorted, but
            // members order is DFS — re-sort by path).
            members.sort_by_key(|&i| nodes[i].as_str());
            sccs.push(members);
        }
    }

    let scc_n = sccs.len();
    let mut scc_cycle = vec![false; scc_n];
    for (ci, members) in sccs.iter().enumerate() {
        // A cycle is a multi-member SCC or a single node importing itself.
        scc_cycle[ci] = members.len() > 1 || self_loop[members[0]];
    }

    // Condensed depends-on graph: edge SCC_a → SCC_b if some a→b import
    // (a depends on b). Bottom-up: process SCCs with out-degree 0 first.
    let mut out_deg = vec![0usize; scc_n];
    let mut rev_cond: Vec<Vec<usize>> = vec![Vec::new(); scc_n]; // b → a (dependents)
    let mut seen_e = HashSet::new();
    for &(f, t) in edges {
        let Some(&fi) = idx.get(f) else { continue };
        let Some(&ti) = idx.get(t) else { continue };
        if fi == ti {
            continue;
        }
        let a = scc_of[fi];
        let b = scc_of[ti];
        if a == b {
            scc_cycle[a] = true;
            continue;
        }
        if seen_e.insert((a, b)) {
            out_deg[a] += 1;
            rev_cond[b].push(a);
        }
    }

    // Kahn bottom-up: start with out_deg == 0, stable by min path in SCC.
    let scc_key = |ci: usize| -> &str { nodes[sccs[ci][0]].as_str() };
    let mut ready: BTreeSet<(String, usize)> = BTreeSet::new();
    for (ci, &deg) in out_deg.iter().enumerate() {
        if deg == 0 {
            ready.insert((scc_key(ci).to_string(), ci));
        }
    }
    let mut scc_order = Vec::with_capacity(scc_n);
    while let Some((_, ci)) = ready.pop_first() {
        scc_order.push(ci);
        for &dep in &rev_cond[ci] {
            out_deg[dep] -= 1;
            if out_deg[dep] == 0 {
                ready.insert((scc_key(dep).to_string(), dep));
            }
        }
    }
    // Cycle in condensed graph (shouldn't happen post-SCC) — append rest path-asc.
    if scc_order.len() < scc_n {
        let mut rest: Vec<usize> = (0..scc_n).filter(|c| !scc_order.contains(c)).collect();
        rest.sort_by_key(|&ci| scc_key(ci));
        scc_order.extend(rest);
    }

    let mut out = Vec::with_capacity(n);
    for ci in scc_order {
        let cycle = scc_cycle[ci];
        for &ui in &sccs[ci] {
            out.push((nodes[ui].clone(), cycle));
        }
    }
    out
}

// --- map / order builders --------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MapSymbol {
    pub name: String,
    pub kind: String,
    /// Trust class of the symbol identity (file-level nodes ship empty
    /// `symbols_changed`; when present, class is the resolution class).
    pub class: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MapNode {
    pub path: String,
    pub status: String,
    pub symbols_changed: Vec<MapSymbol>,
    /// `true` when any `session:*` author_stats row exists; `null`
    /// otherwise — absence of session rows is NOT proof no agent touched
    /// the file (the session↔commit join is best-effort and may simply
    /// never have resolved for this path), so `false` is never emitted.
    /// Mirrors `behavioral::routes::ownership_route`'s `agent_share`
    /// semantics ("null when no agent rows at all").
    pub agent_touched: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MapEdge {
    pub from: String,
    pub to: String,
    /// `"import"` | `"call"`.
    pub kind: String,
    pub class: String,
}

fn agent_touched_for(store: &Store, repo_id: i64, path: &str) -> Option<bool> {
    let rows = store.author_stats_for(repo_id, path).ok()?;
    // No session-authored row ⇒ UNKNOWN, not false: the session↔commit
    // join is best-effort, so human-only rows don't prove agent absence
    // (same rule as ownership_route's agent_share).
    if rows.iter().any(|r| is_session_author(&r.author)) {
        Some(true)
    } else {
        None
    }
}

fn inputs_missing_for(store: &Store, repo_id: i64) -> Vec<String> {
    let mut missing = Vec::new();
    let edges = store.import_edge_count_for_repo(repo_id).unwrap_or(0);
    let specs = store.import_spec_count_for_repo(repo_id).unwrap_or(0);
    // Extraction hasn't landed when neither specs nor resolved edges exist.
    if edges == 0 && specs == 0 {
        missing.push("import_edges".into());
    }
    let calls = store.call_site_count_for_repo(repo_id).unwrap_or(0);
    if calls == 0 {
        missing.push("call_sites".into());
    }
    missing
}

fn build_map_sync(
    store: &Store,
    repo_id: i64,
    review_id: i64,
    repo_name: &str,
    ps_number: i64,
    files: &[FileChange],
) -> Result<serde_json::Value, ApiError> {
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let import_pairs = store
        .import_edges_among_paths(repo_id, &paths)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let call_pairs = store
        .call_edges_among_paths(repo_id, &paths)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut nodes: Vec<MapNode> = files
        .iter()
        .map(|f| MapNode {
            path: f.path.clone(),
            status: status_wire(&f.status).to_string(),
            // RETURN LESS: file-level nodes only (no hunk∩span symbols).
            symbols_changed: Vec::new(),
            agent_touched: agent_touched_for(store, repo_id, &f.path),
        })
        .collect();
    nodes.sort_by(|a, b| a.path.cmp(&b.path));

    let mut edges: Vec<MapEdge> = Vec::new();
    for (from, to) in &import_pairs {
        edges.push(MapEdge {
            from: from.clone(),
            to: to.clone(),
            kind: "import".into(),
            // Static path resolution to an existing file_id — exact.
            class: CLASS_EXACT.to_string(),
        });
    }
    for (from, to, class) in &call_pairs {
        edges.push(MapEdge {
            from: from.clone(),
            to: to.clone(),
            kind: "call".into(),
            class: (*class).to_string(),
        });
    }
    // Deterministic: kind asc, then from, then to, then class.
    edges.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.from.cmp(&b.from))
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.class.cmp(&b.class))
    });

    let inputs_missing = inputs_missing_for(store, repo_id);

    Ok(serde_json::json!({
        "schema": MAP_SCHEMA,
        "review_id": review_id,
        "repo": repo_name,
        "ps_number": ps_number,
        "nodes": nodes,
        "edges": edges,
        "inputs_missing": inputs_missing,
        "note": "Static dependency skeleton over the latest patchset's \
                 changed files only — no transitive closure. Import edges \
                 are exact (resolved file_id). Call edges are name-unique \
                 among the change set (exact when unique repo-wide, else \
                 likely). symbols_changed is empty at file-level nodes \
                 (span∩hunk deferred). agent_touched is null when no \
                 author_stats row exists.",
    }))
}

fn build_order_sync(
    store: &Store,
    repo_id: i64,
    review_id: i64,
    repo_name: &str,
    ps_number: i64,
    files: &[FileChange],
) -> Result<serde_json::Value, ApiError> {
    let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
    let import_pairs = store
        .import_edges_among_paths(repo_id, &paths)
        .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let file_rows: Vec<(String, String)> = files
        .iter()
        .map(|f| (f.path.clone(), status_wire(&f.status).to_string()))
        .collect();
    // Stable input order for compute: path-asc (status taken from map).
    let mut by_path: BTreeMap<String, String> = BTreeMap::new();
    for (p, s) in file_rows {
        by_path.insert(p, s);
    }
    let sorted: Vec<(String, String)> = by_path.into_iter().collect();
    let stops = compute_reading_order(&sorted, &import_pairs);
    let inputs_missing = inputs_missing_for(store, repo_id);

    Ok(serde_json::json!({
        "schema": ORDER_SCHEMA,
        "review_id": review_id,
        "repo": repo_name,
        "ps_number": ps_number,
        "stops": stops,
        "inputs_missing": inputs_missing,
    }))
}

// --- routes ----------------------------------------------------------------

/// `GET /api/reviews/{id}/map` — change-set dependency skeleton over the
/// latest patchset (same selection as `/files` with no `?ps=`).
pub async fn review_map_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;
    let ps = state
        .store
        .run_blocking(move |store| resolve_ps(store, id, None))
        .await?;
    let root = repo.path.clone();
    let base = ps.base_sha.clone();
    let tip = ps.tip_sha.clone();
    let store = state.store.clone();
    let repo_name = review.repo.clone();
    let ps_number = ps.ps_number;

    let out = tokio::task::spawn_blocking(move || {
        let files = files_changed(&root, &base, &tip)?;
        build_map_sync(&store, repo_id, id, &repo_name, ps_number, &files)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

/// `GET /api/reviews/{id}/reading-order` — deterministic tour of the
/// latest patchset's change set (bottom-up over the import DAG).
pub async fn review_reading_order_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let (review, repo, repo_id) = require_review(&state, id).await?;
    let ps = state
        .store
        .run_blocking(move |store| resolve_ps(store, id, None))
        .await?;
    let root = repo.path.clone();
    let base = ps.base_sha.clone();
    let tip = ps.tip_sha.clone();
    let store = state.store.clone();
    let repo_name = review.repo.clone();
    let ps_number = ps.ps_number;

    let out = tokio::task::spawn_blocking(move || {
        let files = files_changed(&root, &base, &tip)?;
        build_order_sync(&store, repo_id, id, &repo_name, ps_number, &files)
    })
    .await
    .map_err(|e| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))??;

    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

// --- unit tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_detection() {
        assert!(is_test_path("tests/foo.rs"));
        assert!(is_test_path("src/tests/foo.rs"));
        assert!(is_test_path("test/bar.py"));
        assert!(is_test_path("e2e/login.spec.ts"));
        assert!(is_test_path("pkg/foo_test.go"));
        assert!(is_test_path("pkg/foo.test.ts"));
        assert!(is_test_path("pkg/foo.spec.js"));
        assert!(!is_test_path("src/lib.rs"));
        assert!(!is_test_path("testing/helpers.rs"));
        assert!(!is_test_path("src/contest/mod.rs"));
    }

    #[test]
    fn status_wire_mapping() {
        assert_eq!(status_wire("A"), "added");
        assert_eq!(status_wire("M"), "modified");
        assert_eq!(status_wire("D"), "deleted");
        assert_eq!(status_wire("R100"), "renamed");
        assert_eq!(status_wire("C50"), "renamed");
    }

    #[test]
    fn topo_order_dag_dependencies_first() {
        // c imports b, b imports a  →  a, b, c
        let files = vec![
            ("c.rs".into(), "modified".into()),
            ("a.rs".into(), "modified".into()),
            ("b.rs".into(), "modified".into()),
        ];
        let edges = vec![
            ("c.rs".into(), "b.rs".into()),
            ("b.rs".into(), "a.rs".into()),
        ];
        let stops = compute_reading_order(&files, &edges);
        let paths: Vec<&str> = stops.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "b.rs", "c.rs"]);
        assert!(stops.iter().all(|s| !s.cycle));
        assert_eq!(stops[0].reason, "imported by 1 changed files");
        assert_eq!(stops[1].reason, "imported by 1 changed files");
        assert_eq!(stops[2].reason, "no dependency signal");
    }

    #[test]
    fn topo_order_cycle_flagged_and_stable() {
        // a ↔ b cycle, c imports a  →  {a,b} as unit (path-asc), then c
        let files = vec![
            ("c.rs".into(), "modified".into()),
            ("b.rs".into(), "modified".into()),
            ("a.rs".into(), "modified".into()),
        ];
        let edges = vec![
            ("a.rs".into(), "b.rs".into()),
            ("b.rs".into(), "a.rs".into()),
            ("c.rs".into(), "a.rs".into()),
        ];
        let stops = compute_reading_order(&files, &edges);
        let paths: Vec<&str> = stops.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "b.rs", "c.rs"]);
        assert!(stops[0].cycle, "a in cycle");
        assert!(stops[1].cycle, "b in cycle");
        assert!(!stops[2].cycle, "c not in cycle");
    }

    #[test]
    fn tests_last_deleted_before_tests() {
        let files = vec![
            ("z_test.rs".into(), "modified".into()),
            ("gone.rs".into(), "deleted".into()),
            ("lib.rs".into(), "modified".into()),
            ("tests/more.rs".into(), "added".into()),
        ];
        let edges: Vec<(String, String)> = vec![];
        let stops = compute_reading_order(&files, &edges);
        let paths: Vec<&str> = stops.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["lib.rs", "gone.rs", "tests/more.rs", "z_test.rs"]
        );
        assert_eq!(stops[0].reason, "no dependency signal");
        assert_eq!(stops[1].reason, "deleted");
        assert_eq!(stops[2].reason, "test");
        assert_eq!(stops[3].reason, "test");
    }

    #[test]
    fn determinism_twice_identical() {
        let files = vec![
            ("m.rs".into(), "modified".into()),
            ("a.rs".into(), "modified".into()),
            ("b.rs".into(), "modified".into()),
            ("t_test.rs".into(), "added".into()),
            ("old.rs".into(), "deleted".into()),
        ];
        let edges = vec![
            ("m.rs".into(), "a.rs".into()),
            ("m.rs".into(), "b.rs".into()),
            ("a.rs".into(), "b.rs".into()),
        ];
        let s1 = compute_reading_order(&files, &edges);
        let s2 = compute_reading_order(&files, &edges);
        assert_eq!(s1, s2);
        // Also independent of input shuffle:
        let mut files2 = files.clone();
        files2.reverse();
        let s3 = compute_reading_order(&files2, &edges);
        assert_eq!(s1, s3);
    }

    #[test]
    fn unconnected_path_asc_among_ready() {
        // No edges: pure path-asc for live non-test.
        let files = vec![
            ("z.rs".into(), "modified".into()),
            ("a.rs".into(), "modified".into()),
            ("m.rs".into(), "modified".into()),
        ];
        let stops = compute_reading_order(&files, &[]);
        let paths: Vec<&str> = stops.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(paths, vec!["a.rs", "m.rs", "z.rs"]);
    }
}
