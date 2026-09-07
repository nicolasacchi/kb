//! `kbc-trail/1` retention — the paged, background sweep (V74-L3b, D17).
//!
//! D17 permits a server-side attention ledger only in the milestone that
//! ships pause, purge and RETENTION together, so this module is not a
//! follow-up: it is one of the three preconditions for `trail_steps`
//! existing at all.
//!
//! The SHAPE is `lanes::gc`'s, verbatim, and that shape is not a choice —
//! it is V72-B0's two rules, which exist because the stale-salt sweep once
//! cost a production boot its listener:
//!
//! * **(a) Nothing whole-corpus sits between `Store::open` and the bind.**
//!   [`spawn_trail_retention_gc`] is spawned and never awaited.
//! * **(b) A background pass stays PAGED.** The store has ONE connection
//!   mutex, so an hours-long transaction on a background thread does not
//!   fix an outage, it moves it from "never binds" to "binds and answers
//!   nothing". Each page is [`crate::store::TRAIL_GC_PAGE`] trails in its
//!   own short transaction, with a yield between pages.
//!
//! Like the lane sweep and unlike the salt sweep, this one is PERIODIC and
//! needs no on-disk marker: the cutoff is recomputed from the clock on
//! every page, so an interrupted pass resumes at the next tick with
//! nothing lost. A pass is bounded by [`MAX_PAGES_PER_PASS`] so a
//! pathological backlog cannot turn one tick into an unbounded loop.
//!
//! Two ways this task does not start, both of them the honest reading of
//! the config rather than a silent default:
//!
//! * `[trails] enabled = false` (the DEFAULT) — the feature is off, so
//!   there is nothing to age out and no reason to wake up.
//! * `retention_days = 0` — the operator's explicit "keep everything".
//!   `GET /api/trails/state` reports the number, so a reader can see that
//!   nothing will ever be swept.

use crate::config::TrailsSection;
use crate::store::{Store, TRAIL_GC_PAGE};
use std::sync::Arc;
use std::time::Duration;

/// How often a pass runs. Six hours, `lanes::gc::INTERVAL`'s value for
/// `lanes::gc::INTERVAL`'s reason: retention windows are measured in days,
/// so anything finer is I/O for no behaviour change.
pub const INTERVAL: Duration = Duration::from_secs(6 * 3600);
/// Yield between pages so the store's single connection mutex is genuinely
/// released.
pub const PAGE_PAUSE: Duration = Duration::from_millis(50);
/// Upper bound on pages in ONE pass.
pub const MAX_PAGES_PER_PASS: usize = 512;

/// The unix second before which a trail is expired, or `None` when nothing
/// ever expires. Pure, so the boundary is testable without a clock.
pub fn retention_cutoff(cfg: &TrailsSection, now: i64) -> Option<i64> {
    if !cfg.enabled || cfg.retention_days == 0 {
        return None;
    }
    Some(now - (cfg.retention_days as i64) * 86_400)
}

/// Spawn the periodic sweep. Called from `bind_and_spawn` BEFORE the
/// listener binds and never awaited; returns immediately, and returns
/// WITHOUT spawning anything when retention can never expire a row.
pub fn spawn_trail_retention_gc(store: Arc<Store>, cfg: TrailsSection) {
    if retention_cutoff(&cfg, 0).is_none() {
        return;
    }
    tokio::spawn(async move {
        loop {
            let ((trails, steps), pages) = run_pass(&store, &cfg).await;
            if trails > 0 || steps > 0 {
                tracing::info!(
                    trails,
                    steps,
                    pages,
                    retention_days = cfg.retention_days,
                    "trails: retention sweep removed expired trails"
                );
            }
            tokio::time::sleep(INTERVAL).await;
        }
    });
}

/// One full pass: pages until nothing more is expired, the page bound is
/// hit, or the store errors. Returns what it removed and how many pages it
/// took (both surfaced in the log line, and asserted by tests).
pub async fn run_pass(store: &Arc<Store>, cfg: &TrailsSection) -> ((usize, usize), usize) {
    let mut trails = 0usize;
    let mut steps = 0usize;
    let mut pages = 0usize;
    while pages < MAX_PAGES_PER_PASS {
        let Some(cutoff) = retention_cutoff(cfg, chrono::Utc::now().timestamp()) else {
            break;
        };
        let s = store.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            s.sweep_trail_retention_page(cutoff, TRAIL_GC_PAGE)
        })
        .await;
        pages += 1;
        let ((t, st), more) = match outcome {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                // Never fatal: a sweep that cannot run is a store that keeps
                // some expired rows, not a daemon that stops.
                tracing::warn!(error = %e, "trails: retention sweep page failed");
                break;
            }
            Err(e) => {
                tracing::warn!(error = %e, "trails: retention sweep task failed");
                break;
            }
        };
        trails += t;
        steps += st;
        if !more {
            break;
        }
        tokio::time::sleep(PAGE_PAUSE).await;
    }
    ((trails, steps), pages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NewTrail, NewTrailStep};

    fn cfg(enabled: bool, days: u32) -> TrailsSection {
        TrailsSection {
            enabled,
            retention_days: days,
            step_granularity_secs: 1,
        }
    }

    #[test]
    fn a_disabled_ledger_and_a_zero_window_both_expire_nothing() {
        assert_eq!(retention_cutoff(&cfg(false, 30), 1_000_000), None);
        assert_eq!(
            retention_cutoff(&cfg(true, 0), 1_000_000),
            None,
            "retention_days = 0 is the operator's explicit keep-everything"
        );
        assert_eq!(
            retention_cutoff(&cfg(true, 1), 1_000_000),
            Some(1_000_000 - 86_400)
        );
    }

    fn store() -> (tempfile::TempDir, Arc<Store>, i64) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = Store::open(&tmp.path().join("index.db")).expect("open");
        let repo_id = store
            .upsert_repo("r", &tmp.path().display().to_string())
            .expect("repo");
        (tmp, Arc::new(store), repo_id)
    }

    fn seed_trail(store: &Store, repo_id: i64, created: i64, steps: usize) -> String {
        let id = store
            .create_trail(
                repo_id,
                &NewTrail {
                    origin: crate::trails::ORIGIN_RECORDED.into(),
                    title: None,
                    parent_id: None,
                    parent_ordinal: None,
                    session_hint: None,
                },
                created,
            )
            .expect("create");
        let batch: Vec<NewTrailStep> = (0..steps)
            .map(|i| NewTrailStep {
                via: "manual".into(),
                path: Some(format!("a{i}.rb")),
                line_start: None,
                line_end: None,
                symbol: None,
                blob_sha: None,
                entered_at: created,
                dwell_secs: 3,
                day: crate::trails::day_of(created),
                note: None,
            })
            .collect();
        store
            .append_trail_steps(&id, &batch, created)
            .expect("append");
        id
    }

    #[tokio::test]
    async fn a_pass_pages_and_removes_only_what_is_past_the_window() {
        let (_tmp, store, repo_id) = store();
        let now = chrono::Utc::now().timestamp();
        // Two expired (created 40 days ago), one fresh.
        seed_trail(&store, repo_id, now - 40 * 86_400, 3);
        seed_trail(&store, repo_id, now - 39 * 86_400, 2);
        let fresh = seed_trail(&store, repo_id, now - 86_400, 1);

        let ((trails, steps), pages) = run_pass(&store, &cfg(true, 30)).await;
        assert_eq!(trails, 2, "only the two past the 30-day window");
        assert_eq!(steps, 5);
        assert!(pages >= 1);
        assert!(
            store.get_trail(repo_id, &fresh).expect("get").is_some(),
            "a trail inside the window survives"
        );
        // A second pass over the same store is a clean no-op.
        let ((trails, steps), _) = run_pass(&store, &cfg(true, 30)).await;
        assert_eq!((trails, steps), (0, 0));
    }

    #[tokio::test]
    async fn a_disabled_ledger_sweeps_nothing_even_with_expired_rows_present() {
        let (_tmp, store, repo_id) = store();
        let now = chrono::Utc::now().timestamp();
        let old = seed_trail(&store, repo_id, now - 400 * 86_400, 2);
        let ((trails, steps), pages) = run_pass(&store, &cfg(false, 30)).await;
        assert_eq!((trails, steps, pages), (0, 0, 0));
        assert!(
            store.get_trail(repo_id, &old).expect("get").is_some(),
            "turning the feature OFF must not become a silent delete of what it recorded \
             while it was on — purge is the explicit, audited verb for that"
        );
    }
}
