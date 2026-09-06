//! Async-worker-starvation regression test.
//!
//! Ties directly to the 2026-08-31 prod incident documented in the module
//! doc of `crates/kb-code-server/src/store.rs`: `Store` wraps ONE
//! `rusqlite::Connection` behind a `std::sync::Mutex`. Before the
//! `StoreBlocking::run_blocking` fix, a route handler that called a `Store`
//! method INLINE inside an `async fn` blocked its tokio async-worker thread
//! for the whole mutex-wait — during a heavy sink-reconcile burst, enough
//! concurrent store-touching probes could saturate EVERY async worker, so
//! the runtime could no longer poll ANY task, including `/healthz` (which
//! touches no store state at all). That is how kbc.example.com went dark on
//! three consecutive v0.40 deploy attempts while sitting at near-zero CPU —
//! worker starvation, not a deadlock.
//!
//! This test reproduces that exact shape on a purpose-built 2-worker
//! runtime: it holds the store's real connection mutex for 4s (simulating a
//! slow sink write, via `Store::hold_lock_for_test` — a `#[doc(hidden)]`
//! method that exists solely for this test) while 8 concurrent
//! store-touching `GET /api/identity` requests are in flight, then checks
//! that `GET /healthz` still answers within 3s. Pre-fix, every async worker
//! blocks inline on the held mutex and this test times out; post-fix, the
//! store calls are parked on tokio's blocking pool instead, leaving both
//! async workers free to poll `/healthz`.
//!
//! Runs from a plain `#[test]` main thread (not `#[tokio::test]`) so the
//! test controls exactly which OS threads make up the runtime under load —
//! a harness-managed multi-thread runtime would hand us more than 2 workers
//! and hide the bug.
//!
//! `GET /api/identity` is the store-touching probe (not merely "any GET"):
//! its per-repo loop (`routes::identity`) calls `state.store.file_count`/
//! `state.store.symbol_count_for_repo`, but ONLY when at least one repo is
//! configured — a bare `KbCodeConfig::default()` (zero repos) makes that
//! loop a no-op and the route never touches the store at all. This test
//! therefore configures exactly one (empty, `git init`-only) repo, the same
//! minimal fixture `boot_e2e/boot.rs`'s `identity_reports_configured_repos`
//! test uses.
//!
//! `kb_code_server::build_state_for_test` (the crate's own `#[cfg(test)]
//! pub(crate)` full-`SharedState` fixture builder in `src/lib.rs`) is
//! invisible from an integration-test binary — `#[cfg(test)]` items never
//! make it into the rlib that `cargo test` links into `tests/*.rs`
//! binaries, and it's `pub(crate)` besides. `boot_state` below is a
//! deliberate, minimal transplant of that function's body: every call it
//! makes is `pub` API, and the ONLY structural change is threading in an
//! already-open `Arc<Store>` instead of opening a second, independent one —
//! this test needs to hold the EXACT mutex `router::build_router`'s routes
//! acquire, and a second `Store::open` on the same sqlite file would be a
//! completely separate `std::sync::Mutex`, so it would never contend and
//! the test would pass unconditionally regardless of whether the fix is
//! applied. See `build_state_for_test` itself if this ever needs to grow.

use std::net::SocketAddr;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

mod common;

use kb_code_server::config::{KbCodeConfig, RepoEntry};
use kb_code_server::state::SharedState;
use kb_code_server::store::Store;
use kb_core::paths::KbPaths;

use crate::common::git;

/// Transplanted from `kb_code_server::build_state_for_test` (see the module
/// doc above for why an integration test can't call that fn directly).
/// Takes an already-open `store` instead of opening its own, so the caller
/// can hold a clone of the exact `Arc<Store>` this state (and the router
/// built from it) will use.
async fn boot_state(config: KbCodeConfig, store: Arc<Store>) -> anyhow::Result<SharedState> {
    use anyhow::Context;
    use kb_code_server::state::AppState;

    anyhow::ensure!(
        !config.semantic.enabled,
        "boot_state does not support [semantic] enabled = true"
    );

    let mut repo_ids = std::collections::HashMap::with_capacity(config.repos.len());
    for repo in &config.repos {
        let id = store
            .upsert_repo(&repo.name, &repo.path.to_string_lossy())
            .with_context(|| format!("register repo {:?} in the kb-code store", repo.name))?;
        repo_ids.insert(repo.name.clone(), id);
    }
    if let Err(e) = kb_code_server::doclens::pins::prune_stale_pins(&store, &config.repos) {
        tracing::warn!(error = %e, "doc-lens: boot pin prune failed");
    }

    let kb_client = Arc::new(kb_code_server::join::kb_client::KbClient::new(
        config.kb_daemon.clone(),
    ));
    let github_client = Arc::new(kb_code_server::github::GithubClient::new(&config.github));
    let lip_registry = Arc::new(kb_code_server::lip::LipRegistry::new(config.intel.clone()));
    let backfill_depth = config.backfill.resolved_depth();

    let is_rails_by_repo: std::collections::HashMap<String, bool> = config
        .repos
        .iter()
        .map(|repo| {
            let auto_detected = kb_code_server::frameworks::rails::detect_is_rails(&repo.path);
            let enabled = config.rails_lens.repo_enabled(&repo.name, auto_detected);
            (repo.name.clone(), enabled)
        })
        .collect();

    let bus = Arc::new(kb_core::events::EventBus::from_env());
    let (index_sink, _sink_worker) = kb_code_server::sink::spawn(
        store.clone(),
        repo_ids.clone(),
        bus.clone(),
        config.occurrences.clone(),
        is_rails_by_repo,
    );
    let watch_mode = kb_code_server::mirror::parse_watch_mode(&config.watcher.mode);
    let watch_mode_label: &'static str = if watch_mode == kb_code_server::mirror::WatchMode::Poll {
        "polling"
    } else {
        "watching"
    };
    let mirror_config = kb_code_server::mirror::MirrorConfig::new(
        config
            .repos
            .iter()
            .map(|r| kb_code_server::mirror::RepoWatchConfig {
                name: r.name.clone(),
                root: r.path.clone(),
            })
            .collect(),
    )
    .with_mode(watch_mode);
    let watcher = kb_code_server::mirror::MirrorWatcher::start(mirror_config, Arc::new(index_sink))
        .context("start kb-code live-mirror watcher")?;

    let transcripts_root = config.transcripts.resolved_root();
    let transcripts_watcher = if config.transcripts.enabled {
        kb_code_server::transcripts::indexer::TranscriptWatcher::start(
            store.clone(),
            transcripts_root.clone(),
            config.transcripts.exclude_projects.clone(),
            config.transcripts.index_thinking,
        )
        .ok()
        .map(Arc::new)
    } else {
        None
    };

    let auth = Arc::new(kb_server::state::AuthConfig::default());

    let started_at = chrono::Utc::now();
    let scopes = config.scopes.clone();
    let scip_cfg = config.scip.clone();
    let review_cfg = config.review.clone();
    let _auto_capture = kb_code_server::reviews::spawn_auto_capture_worker(
        store.clone(),
        bus.clone(),
        config.repos.clone(),
        review_cfg.max_patchsets,
        review_cfg.patchset_capture,
    );
    let doclens_cfg = config.doclens.clone();
    let behavioral_cfg = config.behavioral.clone();
    let _behavioral_worker = kb_code_server::behavioral::spawn_behavioral_worker(
        store.clone(),
        bus.clone(),
        config.repos.clone(),
        repo_ids.clone(),
        behavioral_cfg.clone(),
    );
    Ok(Arc::new(AppState {
        version: kb_code_server::version(),
        started_at,
        repos: config.repos,
        store,
        repo_ids,
        bus,
        watch_mode: watch_mode_label,
        watcher: Arc::new(watcher),
        file_index: Arc::new(kb_code_server::search::FileIndex::new()),
        symbol_index: Arc::new(kb_code_server::search::SymbolIndex::new()),
        status_index: Arc::new(kb_code_server::git_status::StatusIndex::new()),
        semantic: config.semantic,
        semantic_chunk_store: None,
        semantic_embedder: None,
        semantic_indexer: None,
        transcripts_root,
        transcripts_watcher,
        kb_daemon: config.kb_daemon,
        auth,
        blame_cache: Arc::new(kb_code_server::blame::BlameCache::default()),
        kb_client,
        backfill_depth,
        spa_dist: None,
        github: github_client,
        scopes,
        review: review_cfg,
        behavioral: behavioral_cfg,
        doclens: doclens_cfg,
        doclens_sync_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        scip: scip_cfg,
        lip: lip_registry,
        // V70-A2 — the hardening state `bind_and_spawn` builds; defaults
        // here (this fixture drives `routes::identity`, not a guard).
        host_policy: kb_code_server::security::origin::HostPolicy::default(),
        secret_policy: kb_code_server::security::secrets::SecretPolicy::default(),
        git_fanout: Arc::new(tokio::sync::Semaphore::new(4)),
        scratch_root: std::env::temp_dir().join("kb-code-starvation-scratch"),
    }))
}

/// One empty (`git init`-only, no commit) repo — enough to make
/// `routes::identity`'s per-repo loop call `state.store.file_count`/
/// `symbol_count_for_repo`, matching `boot_e2e/boot.rs`'s
/// `identity_reports_configured_repos` fixture shape exactly.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    tmp
}

#[test]
fn store_mutex_held_does_not_starve_healthz_probe() {
    let repo = fixture_repo();
    let repo_path = std::fs::canonicalize(repo.path()).expect("canonicalize repo path");

    let daemon_tmp = tempfile::tempdir().expect("daemon tempdir");
    let paths = KbPaths::rooted_at(daemon_tmp.path(), "kb-code");
    let db_path = paths.state.join("index.db");
    let store = Arc::new(Store::open(&db_path).expect("open kb-code store"));

    let config = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "starve".to_string(),
            path: repo_path,
        }],
        ..KbCodeConfig::default()
    };

    // A clone for the lock-holder thread — taken BEFORE `store` moves into
    // `boot_state` below (which consumes it into `AppState`).
    let store_for_holder = store.clone();

    let (tx, rx) = mpsc::channel::<()>();

    // A single OS thread owns the whole scenario's SEQUENCING, on its own
    // 2-worker multi-thread runtime. Boot (state + router + real TCP bind)
    // runs on this runtime too, so the server's own tasks compete for the
    // same 2 workers being starved — the same shape as the real daemon
    // under load, not just the test client's requests.
    let requester = thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("2-worker runtime");

        // Phase 1 — boot to a live, listening server, and ONLY THEN hand
        // control back. Deliberately BEFORE the lock is ever held:
        // `boot_state` itself makes inline, pre-fix-style `Store` calls
        // while constructing the fixture state (repo registration, the
        // doc-lens pin prune, the mirror watcher's startup reconcile) —
        // if any of those raced against an already-held lock they'd stall
        // BOOT ITSELF for up to 4s, blowing the 3s budget below and
        // reporting a false starvation failure regardless of whether the
        // fix is applied. Sequencing boot strictly before the hold makes
        // that race structurally impossible rather than merely unlikely.
        let addr = rt.block_on(async move {
            let state = boot_state(config, store)
                .await
                .expect("boot_state (build router + AppState)");
            let router = kb_code_server::router::build_router(state.clone(), state.auth.clone());

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback listener");
            let addr = listener.local_addr().expect("local_addr");
            tokio::spawn(async move {
                // invariant #3 — ConnectInfo MUST be wired via
                // into_make_service_with_connect_info, or auth_bearer sees
                // no peer and fails closed even for loopback callers.
                let _ = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await;
            });
            addr
        });

        // Phase 2 — NOW hold the store's real connection mutex on a plain
        // OS thread, simulating a slow sink write (the incident's actual
        // mechanism). Nested under this thread (rather than a main-thread
        // sibling) so "boot fully first" is structural ordering, not a
        // race to win. The runtime's worker threads (and the axum::serve
        // task spawned in phase 1) keep running across this gap and the
        // `block_on` calls on either side of it — a multi-thread runtime's
        // workers are alive for the runtime's whole lifetime, not just
        // while a particular `block_on` is in progress.
        let holder = thread::spawn(move || {
            store_for_holder.hold_lock_for_test(Duration::from_secs(4));
        });
        // Generous, cheap headroom for the holder thread to actually start
        // and acquire the (uncontended) mutex before phase 3 fires
        // anything at it — acquiring an uncontended std::sync::Mutex is a
        // sub-millisecond op, so this is pure safety margin.
        thread::sleep(Duration::from_millis(50));

        // Phase 3 — 8 concurrent store-touching requests, fired but not
        // awaited yet (each one, pre-fix, would park an async worker on
        // the held store mutex), then the /healthz liveness probe.
        rt.block_on(async move {
            let client = reqwest::Client::new();
            let mut identity_tasks = Vec::with_capacity(8);
            for _ in 0..8 {
                let client = client.clone();
                let url = format!("http://{addr}/api/identity");
                identity_tasks.push(tokio::spawn(async move {
                    let _ = client.get(url).send().await;
                }));
            }

            // Let the 8 requests actually park against the held mutex
            // before probing liveness.
            tokio::time::sleep(Duration::from_millis(200)).await;

            match client.get(format!("http://{addr}/healthz")).send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
                    let _ = tx.send(());
                }
                _ => {}
            }

            for task in identity_tasks {
                let _ = task.await;
            }
        });

        holder.join().expect("lock-holder thread panicked");
    });

    // (d) — success means /healthz answered while the store mutex was HELD
    // and 8 store-touching requests were in flight on a 2-worker runtime.
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(()) => {}
        Err(_) => panic!(
            "async-worker-starvation regression: GET /healthz did not answer within 3s \
             while the store's connection mutex was held for 4s under 8 concurrent \
             store-touching GET /api/identity requests on a 2-worker tokio runtime. \
             Pre-fix behavior (see store.rs's 2026-08-31 incident note): every async \
             worker blocks INLINE on Store's std::sync::Mutex, so the runtime can't poll \
             ANY task — not even /healthz, which touches no store state. If this fires, \
             some Store call reachable from async context is bypassing \
             StoreBlocking::run_blocking."
        ),
    }

    // (e) — bounded join: the holder (nested inside `requester`) always
    // ends at ~4s, and the request-firing phase can't outlive it (its
    // pending store calls unblock the moment the mutex is released), so
    // this can't hang CI either way.
    requester.join().expect("requester thread panicked");
}
