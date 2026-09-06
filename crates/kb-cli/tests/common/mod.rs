//! Shared "seed a corpus, then wait for the watcher/indexer to catch up"
//! test support — MI test-hardening (2026-08). Mirrors
//! `kb-server/tests/common/mod.rs` (a separate copy: integration-test
//! binaries in different crates can't share code, and these two crates'
//! test suites use different HTTP/CLI shapes for the same idiom).
//!
//! Root cause this addresses: `notes_cli_links_and_backlinks`/
//! `notes_cli_round_trip_against_daemon` and `cat_read`'s
//! `cat_offline_flag_*`/`cat_via_daemon_*`/`cat_record_flag_*` all seed a
//! tempdir corpus, boot a real daemon, and then assume the indexer has
//! caught up — either via a bare fixed `sleep` with no retry at all
//! (`cat_read.rs`'s `boot()`), or via a poll loop with a HARDCODED short
//! deadline (10s, `notes.rs`'s `wait_indexed` + two inline loops). Under
//! host I/O contention (this box is HDD-RAID5 and shared with other
//! tenants' CI runners) the indexer can still be mid-walk well past either
//! of those windows, which reads as a false-red test failure rather than
//! the timing race it actually is.
//!
//! `poll_until` is the ONE replacement idiom: poll with a mild exponential
//! backoff against a deadline that scales from the `KB_TEST_INDEX_TIMEOUT_SECS`
//! env knob (default 30s — generous; documented here and in
//! docs/architecture-invariants.md), and panic with a message naming WHAT it
//! waited for and for how long (never a bare `assert!`).

#![allow(dead_code)] // not every test binary that includes this module uses every helper.

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Frozen canon fixture used by HTTP-backed CLI integration tests.
pub fn canon_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../corpus/canon")
}

/// Default `[kb.*]` section for a tempdir corpus (no embedder, no extras).
pub fn kb_section(path: PathBuf) -> KbSection {
    KbSection {
        path,
        skip_patterns: Vec::new(),
        ui: UiSection::default(),
        embedding_model: None,
        reranker_model: None,
        chunked_embeddings: false,
        graph_boost: None,
        outbound: None,
        atlas: None,
        templates: BTreeMap::new(),
        memory_scope: None,
        default_search_category: None,
        code_url: None,
        decay_policy: None,
        versions: None,
        reading_progress: None,
        search: Default::default(),
        indexable_extensions: None,
        reconcile_secs: None,
        capture_dir: None,
        resurface: None,
        slo: None,
    }
}

/// Minimal `KbConfig` for an in-process `serve_on_random_port` boot.
pub fn base_config(daemon_name: &str, kb: BTreeMap<KbName, KbSection>) -> KbConfig {
    KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.to_string()),
        },
        server: ServerSection::default(),
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb,
        projects: Default::default(),
        sessions: Default::default(),
    }
}

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

/// Sync variant of `poll_until` for call sites that shell out to the `kb`
/// binary (`assert_cmd::Command`) rather than hitting the daemon's HTTP API
/// directly — `cat_read.rs`'s `find_id` runs `kb find`, a blocking
/// subprocess call, so there's no `Future` to poll. Same backoff/deadline/
/// panic-message contract as `poll_until`.
pub fn poll_until_sync<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let budget = index_wait_deadline();
    let deadline = Instant::now() + budget;
    let mut backoff = Duration::from_millis(50);
    const MAX_BACKOFF: Duration = Duration::from_millis(500);
    loop {
        if let Some(v) = check() {
            return v;
        }
        let now = Instant::now();
        if now >= deadline {
            panic!("timed out after {budget:?} waiting for: {what}");
        }
        std::thread::sleep(backoff.min(deadline - now));
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// The per-request HTTP timeout string to hand a spawned `kb` subprocess via
/// `Command::env("KB_TEST_HTTP_TIMEOUT_SECS", …)` — MI test-hardening
/// (2026-08), a SEPARATE knob from `index_wait_deadline` on purpose: that one
/// bounds an outer polling loop running in THIS (the test) process; this one
/// bounds a single outbound HTTP call made by the CHILD `kb` process, whose
/// call-site-hardcoded timeouts (5-600s, see `kb-cli/src/commands/*.rs`) are
/// otherwise too short to survive a daemon that's merely slow — not stuck —
/// under host I/O contention. `notes_cli_links_and_backlinks` hit exactly
/// this: `kb notes new`'s own request timed out well before the daemon's
/// single-writer storage actor got to it, which is a different failure from
/// anything `poll_until` covers (that only starts polling AFTER the mutating
/// call already returned). Reading this env var is a plain, safe
/// `std::env::var` inside the freshly-spawned `kb` process (see
/// `client_with_timeout_and_bearer`'s override) — passing it via
/// `Command::env` on the PARENT side never touches this test binary's own
/// (shared, multi-threaded) process environment, so there's no
/// `std::env::set_var` data-race hazard on either side.
pub fn http_timeout_secs() -> String {
    std::env::var("KB_TEST_HTTP_TIMEOUT_SECS").unwrap_or_else(|_| "30".to_string())
}
