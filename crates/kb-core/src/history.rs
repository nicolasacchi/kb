//! Per-kb history rings — runs + queries. Bounded ring buffers fed
//! from the event firehose so the daemon can answer "what indexing
//! has happened recently?" and "what searches did this kb get?"
//! without making the caller subscribe to SSE.
//!
//! Why dedicated rings vs. snapshotting the firehose: the firehose
//! holds all event kinds in a single 1024-entry ring. After a busy
//! period it can roll past the start of older runs / queries that the
//! TUI still wants to show. These rings keep 256 of each kind
//! per-kb, scoped to the structured shape we render in the UI.
//!
//! Lifetime: in-process; reset on daemon restart. The TUI cold-loads
//! from these endpoints on connect, then keeps up via SSE deltas.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Mutex;

pub const DEFAULT_CAPACITY: usize = 256;

/// One row in the runs sub-tab. A run is born when `index.start`
/// fires and resolves when `index.complete` lands; if the daemon
/// shuts down mid-run the row stays `Running` forever (next boot
/// starts with an empty ring anyway).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunEntry {
    pub run: String,
    pub src: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub ok_count: u64,
    pub err_count: u64,
    pub duration_ms: Option<u64>,
    pub status: RunStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QueryEntry {
    pub at: DateTime<Utc>,
    pub q: String,
    pub mode: String,
    pub hits: u64,
    pub ms: u64,
}

/// Ring of run rows for one kb. `index.start` upserts a `Running`
/// row; `index.complete` resolves the matching row to `Complete`.
/// Newest entries live at the back.
#[derive(Debug)]
pub struct RunsRing {
    inner: Mutex<VecDeque<RunEntry>>,
    capacity: usize,
}

impl Default for RunsRing {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl RunsRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            capacity,
        }
    }

    /// Insert a Running row for this `run` id. If a row with this id
    /// is already present, leave it alone (the firehose may replay).
    pub fn push_start(
        &self,
        run: impl Into<String>,
        src: Option<String>,
        started_at: DateTime<Utc>,
    ) {
        let run = run.into();
        let mut ring = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if ring.iter().any(|e| e.run == run) {
            return;
        }
        if ring.len() == self.capacity {
            ring.pop_front();
        }
        ring.push_back(RunEntry {
            run,
            src,
            started_at,
            finished_at: None,
            ok_count: 0,
            err_count: 0,
            duration_ms: None,
            status: RunStatus::Running,
        });
    }

    /// Resolve the row with this `run` id to Complete. No-op if the
    /// row isn't present (a completion may arrive after the ring has
    /// rolled past its start, especially in tests).
    pub fn resolve_complete(
        &self,
        run: &str,
        finished_at: DateTime<Utc>,
        ok_count: u64,
        err_count: u64,
        duration_ms: u64,
    ) {
        let mut ring = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(row) = ring.iter_mut().find(|e| e.run == run) {
            row.finished_at = Some(finished_at);
            row.ok_count = ok_count;
            row.err_count = err_count;
            row.duration_ms = Some(duration_ms);
            row.status = RunStatus::Complete;
        }
    }

    /// Newest-first snapshot of up to `limit` rows.
    pub fn snapshot(&self, limit: usize) -> Vec<RunEntry> {
        let ring = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        ring.iter().rev().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Ring of query rows for one kb. Each `query` SSE envelope pushes
/// one row; newest at the back.
#[derive(Debug)]
pub struct QueriesRing {
    inner: Mutex<VecDeque<QueryEntry>>,
    capacity: usize,
}

impl Default for QueriesRing {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }
}

impl QueriesRing {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
            capacity,
        }
    }

    pub fn push(
        &self,
        at: DateTime<Utc>,
        q: impl Into<String>,
        mode: impl Into<String>,
        hits: u64,
        ms: u64,
    ) {
        let mut ring = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if ring.len() == self.capacity {
            ring.pop_front();
        }
        ring.push_back(QueryEntry {
            at,
            q: q.into(),
            mode: mode.into(),
            hits,
            ms,
        });
    }

    /// Newest-first snapshot of up to `limit` rows.
    pub fn snapshot(&self, limit: usize) -> Vec<QueryEntry> {
        let ring = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        ring.iter().rev().take(limit).cloned().collect()
    }

    /// Every row currently held (bounded by `capacity`, so at most
    /// [`DEFAULT_CAPACITY`] in practice). GC-B3 — zero-hit aggregation
    /// needs the full ring, not the top-`limit` slice `snapshot` hands
    /// the raw-list route.
    pub fn snapshot_all(&self) -> Vec<QueryEntry> {
        self.snapshot(self.capacity)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// GC-B3 — one normalized zero-hit query, aggregated across every
/// [`QueryEntry`] whose `hits == 0` in a ring snapshot. `query` is the
/// output of [`normalize_query`]; `count` is how many ring rows
/// collapsed into it; `last_seen` is the newest `at` among them.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZeroHitGroup {
    pub query: String,
    pub count: u64,
    pub last_seen: DateTime<Utc>,
}

/// Normalize a query string for zero-hit grouping so `"  Borrow  "`,
/// `"borrow"`, and `"borrow\tchecker"` / `"borrow checker"` collapse to
/// the same key: trim, collapse internal whitespace runs to a single
/// space, lowercase. Corpus-gap signal only — never used for ranking.
pub fn normalize_query(q: &str) -> String {
    q.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Group the zero-hit rows of `entries` by [`normalize_query`], keeping
/// groups with `count >= min_count.max(1)` (a `min_count` of 0 is
/// treated as 1 — a group can't exist with zero occurrences). Sorted by
/// count descending, ties broken by the normalized query text
/// ascending — deterministic regardless of ring insertion order
/// (invariant #1's ORDER-determinism spirit). Rows with `hits != 0` or
/// an empty normalized query are dropped (an empty query carries no
/// corpus-gap signal).
pub fn zero_hit_groups(entries: &[QueryEntry], min_count: u64) -> Vec<ZeroHitGroup> {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<String, (u64, DateTime<Utc>)> = BTreeMap::new();
    for e in entries {
        if e.hits != 0 {
            continue;
        }
        let norm = normalize_query(&e.q);
        if norm.is_empty() {
            continue;
        }
        let slot = groups.entry(norm).or_insert((0, e.at));
        slot.0 += 1;
        if e.at > slot.1 {
            slot.1 = e.at;
        }
    }

    let floor = min_count.max(1);
    let mut out: Vec<ZeroHitGroup> = groups
        .into_iter()
        .filter(|(_, (count, _))| *count >= floor)
        .map(|(query, (count, last_seen))| ZeroHitGroup {
            query,
            count,
            last_seen,
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.query.cmp(&b.query)));
    out
}

/// Apply one event envelope to this kb's rings. Returns true if the
/// envelope was relevant (used by tests + the subscriber task's
/// metrics). Unknown types are ignored.
pub fn apply_envelope(
    runs: &RunsRing,
    queries: &QueriesRing,
    env: &crate::types::Envelope,
) -> bool {
    match env.type_.as_str() {
        "index.start" => {
            let Some(run) = env.payload.get("run").and_then(|v| v.as_str()) else {
                return false;
            };
            let src = env
                .payload
                .get("src")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            runs.push_start(run, src, env.ts);
            true
        }
        "index.complete" => {
            let Some(run) = env.payload.get("run").and_then(|v| v.as_str()) else {
                return false;
            };
            let ok = env
                .payload
                .get("ok_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let err = env
                .payload
                .get("err_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let dur = env
                .payload
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            runs.resolve_complete(run, env.ts, ok, err, dur);
            true
        }
        "query" => {
            let q = env
                .payload
                .get("q")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mode = env
                .payload
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let hits = env
                .payload
                .get("hits")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let ms = env.payload.get("ms").and_then(|v| v.as_u64()).unwrap_or(0);
            queries.push(env.ts, q, mode, hits, ms);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Envelope;
    use serde_json::json;

    #[test]
    fn runs_push_start_then_resolve_marks_complete() {
        let ring = RunsRing::default();
        ring.push_start("r-1", Some("src".into()), Utc::now());
        let snap = ring.snapshot(10);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].status, RunStatus::Running);

        ring.resolve_complete("r-1", Utc::now(), 4, 0, 312);
        let snap = ring.snapshot(10);
        assert_eq!(snap[0].status, RunStatus::Complete);
        assert_eq!(snap[0].ok_count, 4);
        assert_eq!(snap[0].duration_ms, Some(312));
    }

    #[test]
    fn runs_push_start_is_idempotent_on_same_run_id() {
        let ring = RunsRing::default();
        ring.push_start("r-1", None, Utc::now());
        ring.push_start("r-1", None, Utc::now());
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn runs_ring_evicts_oldest_at_capacity() {
        let ring = RunsRing::new(3);
        for i in 0..5 {
            ring.push_start(format!("r-{i}"), None, Utc::now());
        }
        let snap = ring.snapshot(10);
        assert_eq!(snap.len(), 3);
        // newest first
        assert_eq!(snap[0].run, "r-4");
        assert_eq!(snap[2].run, "r-2");
    }

    #[test]
    fn queries_push_appends_and_snapshot_reverses() {
        let ring = QueriesRing::default();
        ring.push(Utc::now(), "borrow", "keyword", 3, 12);
        ring.push(Utc::now(), "macro", "hybrid", 1, 8);
        let snap = ring.snapshot(10);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].q, "macro");
        assert_eq!(snap[1].q, "borrow");
    }

    #[test]
    fn queries_snapshot_clamps_to_limit() {
        let ring = QueriesRing::default();
        for i in 0..10 {
            ring.push(Utc::now(), format!("q-{i}"), "keyword", 0, 0);
        }
        assert_eq!(ring.snapshot(3).len(), 3);
    }

    #[test]
    fn apply_envelope_routes_to_correct_ring() {
        let runs = RunsRing::default();
        let queries = QueriesRing::default();

        let mut e1 = Envelope::new("index.start", json!({"run": "r-1", "src": "canon"}));
        e1.id = 1;
        assert!(apply_envelope(&runs, &queries, &e1));

        let mut e2 = Envelope::new(
            "index.complete",
            json!({"run": "r-1", "ok_count": 7, "err_count": 0, "duration_ms": 91}),
        );
        e2.id = 2;
        assert!(apply_envelope(&runs, &queries, &e2));

        let mut e3 = Envelope::new(
            "query",
            json!({"q": "vec", "mode": "keyword", "hits": 2, "ms": 5}),
        );
        e3.id = 3;
        assert!(apply_envelope(&runs, &queries, &e3));

        let mut other = Envelope::new("watch.create", json!({"path": "/x"}));
        other.id = 4;
        assert!(!apply_envelope(&runs, &queries, &other));

        let r = runs.snapshot(10);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].status, RunStatus::Complete);
        assert_eq!(r[0].ok_count, 7);

        let q = queries.snapshot(10);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].q, "vec");
    }

    #[test]
    fn resolve_complete_on_missing_run_is_noop() {
        let ring = RunsRing::default();
        ring.resolve_complete("never-started", Utc::now(), 1, 0, 10);
        assert!(ring.is_empty());
    }

    #[test]
    fn queries_snapshot_all_ignores_the_limit_cap() {
        let ring = QueriesRing::new(5);
        for i in 0..5 {
            ring.push(Utc::now(), format!("q-{i}"), "keyword", 0, 0);
        }
        assert_eq!(ring.snapshot_all().len(), 5);
    }

    #[test]
    fn normalize_query_trims_lowercases_and_collapses_whitespace() {
        assert_eq!(normalize_query("  Borrow  "), "borrow");
        assert_eq!(normalize_query("borrow\tchecker"), "borrow checker");
        assert_eq!(normalize_query("Borrow   Checker"), "borrow checker");
        assert_eq!(normalize_query(""), "");
    }

    fn qe(q: &str, hits: u64, at: DateTime<Utc>) -> QueryEntry {
        QueryEntry {
            at,
            q: q.into(),
            mode: "keyword".into(),
            hits,
            ms: 3,
        }
    }

    #[test]
    fn zero_hit_groups_collapses_normalized_duplicates_and_counts() {
        let t0 = Utc::now();
        let entries = vec![
            qe("Borrow", 0, t0),
            qe("  borrow  ", 0, t0 + chrono::Duration::seconds(1)),
            qe("borrow", 5, t0 + chrono::Duration::seconds(2)), // has hits — excluded
            qe("macro", 0, t0),
        ];
        let groups = zero_hit_groups(&entries, 1);
        assert_eq!(groups.len(), 2);
        // sorted by count desc, "borrow" (2) before "macro" (1)
        assert_eq!(groups[0].query, "borrow");
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[0].last_seen, t0 + chrono::Duration::seconds(1));
        assert_eq!(groups[1].query, "macro");
        assert_eq!(groups[1].count, 1);
    }

    #[test]
    fn zero_hit_groups_min_count_filters_singletons() {
        let t0 = Utc::now();
        let entries = vec![qe("borrow", 0, t0), qe("borrow", 0, t0), qe("macro", 0, t0)];
        let groups = zero_hit_groups(&entries, 2);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].query, "borrow");
        assert_eq!(groups[0].count, 2);
    }

    #[test]
    fn zero_hit_groups_min_count_zero_is_treated_as_one() {
        let entries = vec![qe("borrow", 0, Utc::now())];
        assert_eq!(zero_hit_groups(&entries, 0).len(), 1);
    }

    #[test]
    fn zero_hit_groups_drops_empty_and_nonzero_hit_rows() {
        let entries = vec![
            qe("", 0, Utc::now()),
            qe("   ", 0, Utc::now()),
            qe("real query", 3, Utc::now()),
        ];
        assert!(zero_hit_groups(&entries, 1).is_empty());
    }

    #[test]
    fn zero_hit_groups_ties_break_lexicographically() {
        let t0 = Utc::now();
        let entries = vec![qe("zeta", 0, t0), qe("alpha", 0, t0)];
        let groups = zero_hit_groups(&entries, 1);
        assert_eq!(groups[0].query, "alpha");
        assert_eq!(groups[1].query, "zeta");
    }
}
