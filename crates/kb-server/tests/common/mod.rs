//! Shared "seed a corpus, then wait for the watcher/indexer to catch up"
//! test support — MI test-hardening (2026-08).
//!
//! Root cause this addresses: `atlas::tests::*`, `notes_cli_links_and_
//! backlinks`/`notes_cli_round_trip_against_daemon` (kb-cli), `cat_read`'s
//! `cat_offline_flag_*`/`cat_via_daemon_*`/`cat_record_flag_*` (kb-cli), and
//! `api_artifact_route_scrubs_on_non_loopback` (this crate) all seed a
//! tempdir corpus, boot a real daemon, and then assume the indexer/watcher
//! has caught up — either via a bare fixed `sleep` with no retry at all, or
//! via a poll loop with a HARDCODED short deadline (10s). Under host I/O
//! contention (this box is HDD-RAID5 and shared with other tenants' CI
//! runners) the indexer can still be mid-walk well past either of those
//! windows, which reads as a false-red test failure rather than the timing
//! race it actually is.
//!
//! `poll_until` is the ONE replacement idiom: poll with a mild exponential
//! backoff against a deadline that scales from the `KB_TEST_INDEX_TIMEOUT_SECS`
//! env knob (default 30s — generous; documented here and in
//! docs/architecture-invariants.md), and panic with a message naming WHAT it
//! waited for and for how long (never a bare `assert!`).
//!
//! `atlas_lance_lock` is a SEPARATE, unrelated fix for a SEPARATE root cause:
//! `atlas_backfill.rs`/`atlas_points_memo.rs` are this crate's heaviest
//! lance/datafusion consumers (a full corpus scan + PCA per call), and
//! running several of them concurrently — the default per-binary thread
//! concurrency `cargo test` uses — has produced lance/datafusion memory-pool
//! exhaustion under host load, not a timing race. Raising a timeout does
//! nothing for that failure mode; serializing the heavy tests against each
//! other (bounding how many are simultaneously mid-scan) is the actual fix.
//! Scope: this lock is per-TEST-BINARY (a `static` inside a `tests/common`
//! module is compiled fresh into each integration-test binary that includes
//! it). The `memo` harness merges `atlas_backfill` + `atlas_points_memo` +
//! `facets_memo`, so one lock now actually serializes those lance-heavy
//! tests against each other. A test runner that parallelizes ACROSS
//! binaries as separate processes (e.g. `cargo nextest run`) would need a
//! cross-process lock instead; that is out of scope here.

#![allow(dead_code)] // not every test binary that includes this module uses every helper.

use std::future::Future;
use std::path::PathBuf;
use std::process::Child;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// The deadline every "seed a corpus, then poll until the watcher/indexer
/// catches up" call uses, scaled from `KB_TEST_INDEX_TIMEOUT_SECS` (default
/// 30s). Documented in docs/architecture-invariants.md's "Common pitfalls" —
/// bump it via the env var on a slower/more contended box rather than
/// hand-editing a call site.
pub fn index_wait_deadline() -> Duration {
    let secs = std::env::var("KB_TEST_INDEX_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);
    Duration::from_secs(secs)
}

/// Poll `check` until it returns `Some(_)` or `index_wait_deadline()`
/// elapses. Backs off geometrically (50ms → capped at 500ms) between
/// attempts rather than hammering the daemon in a tight loop. On timeout,
/// panics naming `what` was awaited and the exact deadline used — never a
/// bare `assert!` — so a genuine failure and a starved-host race are
/// trivially distinguishable in the test output.
pub async fn poll_until<T, Fut, F>(what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let budget = index_wait_deadline();
    let deadline = Instant::now() + budget;
    let mut backoff = Duration::from_millis(50);
    const MAX_BACKOFF: Duration = Duration::from_millis(500);
    loop {
        if let Some(v) = check().await {
            return v;
        }
        let now = Instant::now();
        if now >= deadline {
            panic!("timed out after {budget:?} waiting for: {what}");
        }
        tokio::time::sleep(backoff.min(deadline - now)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// See the module doc — serializes this binary's lance/datafusion-heavy
/// atlas tests against EACH OTHER so at most one is ever mid-scan at a
/// time. Acquire as the FIRST statement in the test body and hold the guard
/// for the whole test:
/// `let _guard = common::atlas_lance_lock().lock().await;`
pub fn atlas_lance_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Poll `GET /api/identity` until it returns 200.
pub async fn wait_http_up(addr: std::net::SocketAddr) {
    poll_until("GET /api/identity 200", || async move {
        match reqwest::Client::new()
            .get(format!("http://{addr}/api/identity"))
            .send()
            .await
        {
            Ok(r) if r.status().as_u16() == 200 => Some(()),
            _ => None,
        }
    })
    .await
}

/// Poll `GET /api/kb/{kb}/docs` until at least `min` docs are listed.
pub async fn wait_docs_listed(addr: std::net::SocketAddr, kb: &str, min: usize) {
    let kb = kb.to_string();
    let what = format!("{min} docs listed in kb `{kb}`");
    poll_until(&what, || {
        let kb = kb.clone();
        async move {
            let resp = reqwest::Client::new()
                .get(format!("http://{addr}/api/kb/{kb}/docs?limit=200"))
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let docs: Vec<serde_json::Value> = resp.json().await.ok()?;
            (docs.len() >= min).then_some(())
        }
    })
    .await
}

/// Kill-on-drop guard for a spawned `kb-server` child (process harness).
pub struct ServerProc {
    pub child: Child,
    pub stderr_path: PathBuf,
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ServerProc {
    pub fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
