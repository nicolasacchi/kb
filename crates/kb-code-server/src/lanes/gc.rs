//! Lane retention — the paged, background sweep that ages a run and its
//! facts out together (V72-H4a).
//!
//! The design's §P8 asks for a retention policy "from day one", and the
//! resource-governance line calls lane retention out by name on this
//! IO-bound host. The SHAPE of this sweep is not a choice: it is V72-B0's
//! two rules, which exist because the stale-salt sweep once cost a
//! production boot its listener.
//!
//! * **(a) Nothing whole-corpus sits between `Store::open` and the bind.**
//!   [`spawn_lane_retention_gc`] is spawned and never awaited.
//! * **(b) A background pass stays PAGED.** The store has ONE connection
//!   mutex, so an hours-long transaction on a background thread does not
//!   fix an outage, it only moves it from "never binds" to "binds and
//!   answers nothing". Each page is [`crate::store::LANE_GC_PAGE`] runs in
//!   its own short transaction, with a yield between pages.
//!
//! Unlike the salt sweep, this one is PERIODIC (retention is a moving
//! window, not a one-time repair) and needs no on-disk marker: the cutoff
//! is recomputed from the clock on every page, so an interrupted pass
//! simply resumes at the next tick with nothing lost. A pass is also
//! bounded by [`MAX_PAGES_PER_PASS`] so a pathological backlog cannot turn
//! one tick into an unbounded loop.
//!
//! A DISABLED lane is deliberately never swept — see
//! [`super::retention_cutoffs`].

use crate::config::LanesSection;
use crate::store::{Store, LANE_GC_PAGE};
use std::sync::Arc;
use std::time::Duration;

/// How often a pass runs. Six hours: retention windows are measured in
/// days, so anything finer is I/O for no behaviour change.
pub const INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// Yield between pages so the store's single connection mutex is genuinely
/// released (the same reason `spawn_stale_salt_sweep` sleeps per page).
pub const PAGE_PAUSE: Duration = Duration::from_millis(50);
/// Upper bound on pages in ONE pass.
pub const MAX_PAGES_PER_PASS: usize = 512;

/// Spawn the periodic sweep. Called from `bind_and_spawn` BEFORE the
/// listener binds and never awaited; returns immediately.
///
/// A daemon with no ingested lane enabled starts no task at all — the
/// common case for a fresh install, where `[lanes]` is absent entirely.
pub fn spawn_lane_retention_gc(store: Arc<Store>, cfg: LanesSection) {
    if super::retention_cutoffs(&cfg, 0).is_empty() {
        return;
    }
    tokio::spawn(async move {
        loop {
            let (counts, pages) = run_pass(&store, &cfg).await;
            if !counts.is_empty() {
                tracing::info!(
                    runs = counts.runs,
                    facts = counts.facts,
                    pages,
                    "lanes: retention sweep removed expired runs"
                );
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

/// One full pass: pages until nothing more is expired, the page bound is
/// hit, or the store errors. Returns what it removed and how many pages it
/// took (both surfaced in the log line, and asserted by tests).
pub async fn run_pass(
    store: &Arc<Store>,
    cfg: &LanesSection,
) -> (crate::store::LaneGcCounts, usize) {
    let mut total = crate::store::LaneGcCounts::default();
    let mut pages = 0usize;
    while pages < MAX_PAGES_PER_PASS {
        let cutoffs = super::retention_cutoffs(cfg, chrono::Utc::now().timestamp());
        if cutoffs.is_empty() {
            break;
        }
        let s = store.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            s.sweep_lane_retention_page(&cutoffs, LANE_GC_PAGE)
        })
        .await;
        pages += 1;
        let (counts, more) = match outcome {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                // Never fatal: a sweep that cannot run is a store that
                // keeps some expired rows, not a daemon that stops.
                tracing::warn!(error = %e, "lanes: retention sweep page failed");
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "lanes: retention sweep task failed");
                break;
            }
        };
        total.runs += counts.runs;
        total.facts += counts.facts;
        if !more {
            break;
        }
        tokio::time::sleep(PAGE_PAUSE).await;
    }
    (total, pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{LaneFactIn, LaneRunIn};

    fn cfg(enabled: &[&str], days: &[(&str, u32)]) -> LanesSection {
        LanesSection {
            enabled: enabled.iter().map(|s| s.to_string()).collect(),
            retention_days: days.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    fn store() -> (tempfile::TempDir, Arc<Store>, i64) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).expect("open");
        let repo_id = store.upsert_repo("r", "/tmp/r").expect("repo");
        (tmp, Arc::new(store), repo_id)
    }

    fn seed(store: &Store, repo_id: i64, lane: &str, run: &str, ingested_at: i64, facts: usize) {
        let run = LaneRunIn {
            run_id: run.to_string(),
            lane: lane.to_string(),
            repo_id,
            tool: "t".into(),
            tool_version: None,
            argv_redacted: None,
            started_at: None,
            finished_at: None,
            origin: "cli",
            ingested_at,
        };
        let rows: Vec<LaneFactIn> = (0..facts)
            .map(|i| LaneFactIn {
                // A distinct path per fact so the REPLACE arm never
                // collapses this seed into one row.
                path: format!("{run_id}/{i}.rb", run_id = run.run_id),
                blob_sha: "b".into(),
                sha_source: "tool",
                range_start: Some(1),
                range_end: Some(1),
                snippet: Some("x".into()),
                kind: "diagnostic".into(),
                value_json: "{}".into(),
                severity: Some("info".into()),
                produced_at: ingested_at,
            })
            .collect();
        store.replace_lane_facts(&run, &rows, &[]).expect("seed");
    }

    #[tokio::test]
    async fn a_pass_ages_out_expired_runs_and_their_facts_together() {
        let (_tmp, store, repo_id) = store();
        let now = chrono::Utc::now().timestamp();
        seed(&store, repo_id, "rubocop", "old", now - 30 * 86_400, 3);
        seed(&store, repo_id, "rubocop", "new", now - 86_400, 2);

        let (counts, pages) = run_pass(&store, &cfg(&["rubocop"], &[("rubocop", 14)])).await;
        assert_eq!(counts.runs, 1);
        assert_eq!(counts.facts, 3);
        assert_eq!(pages, 1, "one page covers two runs");

        let stats = store.lane_stats(Some(repo_id)).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].runs, 1);
        assert_eq!(stats[0].facts, 2);
    }

    #[tokio::test]
    async fn the_sweep_is_paged_and_resumes_across_pages() {
        let (_tmp, store, repo_id) = store();
        let now = chrono::Utc::now().timestamp();
        // More expired runs than one page holds.
        let n = LANE_GC_PAGE + 5;
        for i in 0..n {
            seed(
                &store,
                repo_id,
                "rubocop",
                &format!("r{i}"),
                now - 30 * 86_400,
                1,
            );
        }
        // A single page removes exactly LANE_GC_PAGE runs and says there
        // is more.
        let cutoffs = super::super::retention_cutoffs(&cfg(&["rubocop"], &[]), now);
        let (page, more) = store
            .sweep_lane_retention_page(&cutoffs, LANE_GC_PAGE)
            .unwrap();
        assert_eq!(page.runs as usize, LANE_GC_PAGE);
        assert!(more);

        // The pass drains the rest.
        let (counts, pages) = run_pass(&store, &cfg(&["rubocop"], &[])).await;
        assert_eq!(counts.runs as usize, 5);
        assert!(pages >= 1);
        assert!(store.lane_stats(Some(repo_id)).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_disabled_lane_is_never_swept_out_from_under_a_re_enable() {
        let (_tmp, store, repo_id) = store();
        let now = chrono::Utc::now().timestamp();
        seed(&store, repo_id, "rubocop", "old", now - 300 * 86_400, 2);
        let (counts, _) = run_pass(&store, &cfg(&["coverage.simplecov"], &[])).await;
        assert!(counts.is_empty());
        assert_eq!(store.lane_stats(Some(repo_id)).unwrap()[0].facts, 2);
    }

    #[tokio::test]
    async fn a_config_with_no_ingested_lane_does_no_work_at_all() {
        let (_tmp, store, _repo_id) = store();
        let (counts, pages) = run_pass(&store, &cfg(&["git.behavior"], &[])).await;
        assert!(counts.is_empty());
        assert_eq!(pages, 0, "a derived-only config never opens a transaction");
    }
}
