//! Bounded, ORDER-PRESERVING concurrent fan-out — kb (the sibling daemon)'s
//! invariant #28 ethos, ported to kb-code.
//!
//! kb-code had no shared helper for this: `doclens::resolve::resolve_scorecard`
//! (the one real fan-out in this crate today — one score pass per configured
//! repo) hand-rolled `futures::stream::iter(..).buffered(N)` inline, with a
//! comment restating kb-server invariant #28 rather than reusing code (see
//! `doclens::SCORECARD_FANOUT_CAP`'s old doc). This module gives that pattern
//! one shared home, mirroring `kb_server::routes::buffered_join` byte-for-byte
//! in behaviour (submission order via `buffered`, never `buffer_unordered`).
//!
//! # The rules (kb-server invariant #28, restated for kb-code)
//!
//! - **Submission order is load-bearing.** [`buffered_join`] is built on
//!   `futures::stream::buffered`, NEVER `buffer_unordered` — the caller's
//!   `Vec` order (config order, request order, …) is preserved in the
//!   output, even though the futures may internally COMPLETE out of order.
//!   Any caller merging per-repo/per-corpus results into an ordered list
//!   (a scorecard, a fan-out search) depends on this.
//! - **Callers return partials and drop-on-error, never `?`-propagate.**
//!   Each future's `Output` is collected unconditionally — one bad repo
//!   must never fail the whole fan-out. Encode failure IN `T` (an error
//!   variant, an `Option`, …) and fold after collection, not by making the
//!   future itself short-circuit with `?`.
//! - **No `std::sync::Mutex` guard may cross an `.await` inside a future
//!   passed here.** Take the guard, extract the owned value, drop the
//!   guard, THEN await — holding a sync mutex across an await point can
//!   deadlock the executor (or at minimum serialize what was meant to be
//!   concurrent).
//!
//! Callers build one boxed future per unit of fan-out (capturing owned
//! clones or `&'f` borrows under one lifetime — see [`buffered_join`]'s own
//! signature) and bake any per-unit identity (a repo name, a kb slug, …)
//! into the future's `Output` so it travels with the result.

use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};

/// Default concurrency cap for [`buffered_join`] when a caller has no more
/// specific reason to pick another number (mirrors kb-server's own
/// `routes::FANOUT_CAP`). Individual call sites are free to use their own
/// cap constant instead — e.g. `doclens::SCORECARD_FANOUT_CAP` (4), sized to
/// that route's own `spawn_blocking` + file-read cost profile — this is
/// just the fallback for a new caller that hasn't measured one yet.
pub const DEFAULT_FANOUT_CAP: usize = 8;

/// Run `futs` concurrently, at most `cap` in flight at once (`cap < 1` is
/// treated as 1 — never zero concurrency), and return their outputs in
/// SUBMISSION order (`futs`'s own order), not completion order. See the
/// module doc for the full contract (order-preservation, drop-on-error,
/// no sync-mutex-across-await).
pub async fn buffered_join<'f, T: Send + 'f>(futs: Vec<BoxFuture<'f, T>>, cap: usize) -> Vec<T> {
    stream::iter(futs).buffered(cap.max(1)).collect().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // invariant:28 submission-order
    #[tokio::test]
    async fn buffered_join_preserves_submission_order_not_completion_order() {
        // Earlier futures sleep LONGER, so completion order is the reverse
        // of submission order. `buffered_join` must still yield in
        // submission order.
        let mut futs: Vec<BoxFuture<'_, u8>> = Vec::new();
        for k in 0u8..6 {
            let ms = (6 - k as u64) * 5;
            futs.push(Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                k
            }));
        }
        let out = buffered_join(futs, 8).await;
        assert_eq!(out, vec![0, 1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn buffered_join_caps_concurrency() {
        // cap=3, N=6 → exactly two full barrier cycles of 3. A Barrier of
        // size `cap` only releases when `cap` futures are simultaneously in
        // flight, proving the fan-out actually runs `cap` concurrently; the
        // timeout converts an under-parallelised (would-deadlock) failure
        // into a clean assertion instead of a hang.
        const CAP: usize = 3;
        let inflight = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(CAP));
        let mut futs: Vec<BoxFuture<'_, ()>> = Vec::new();
        for _ in 0..(CAP * 2) {
            let inflight = inflight.clone();
            let max_seen = max_seen.clone();
            let barrier = barrier.clone();
            futs.push(Box::pin(async move {
                let cur = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(cur, Ordering::SeqCst);
                barrier.wait().await;
                inflight.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        let res = tokio::time::timeout(Duration::from_secs(5), buffered_join(futs, CAP)).await;
        assert!(
            res.is_ok(),
            "buffered_join did not reach cap concurrency within timeout"
        );
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            CAP,
            "concurrency should reach exactly the cap"
        );
    }

    #[tokio::test]
    async fn buffered_join_fold_drops_failing_units() {
        // Each future returns (k, Result); the CALLER folds Ok and drops
        // Err — the per-unit skip-on-error isolation every real caller
        // relies on (buffered_join itself never short-circuits).
        let mut futs: Vec<BoxFuture<'_, (u8, Result<u8, &'static str>)>> = Vec::new();
        for (k, ok) in [(0u8, true), (1, false), (2, true)] {
            futs.push(Box::pin(async move {
                (k, if ok { Ok(k) } else { Err("boom") })
            }));
        }
        let out = buffered_join(futs, 8).await;
        let survivors: Vec<u8> = out.into_iter().filter_map(|(_, r)| r.ok()).collect();
        assert_eq!(survivors, vec![0, 2]);
    }

    #[tokio::test]
    async fn buffered_join_zero_cap_still_makes_progress() {
        let futs: Vec<BoxFuture<'_, u8>> = vec![Box::pin(async { 1u8 }), Box::pin(async { 2u8 })];
        let out = buffered_join(futs, 0).await;
        assert_eq!(out, vec![1, 2]);
    }
}
