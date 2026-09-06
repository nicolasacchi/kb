//! kb-server — axum HTTP+SSE daemon. `serve_with_paths` builds the router,
//! spawns one storage actor + one watcher + one indexer per kb, binds the
//! listener, and runs `axum::serve(...).await`, returning a `ServeOutcome`.
//!
//! `serve_loop` wraps it in the boot loop that re-reads the config + rebuilds
//! on a `PUT /api/config` restart (rolling back to last-good on a failed
//! boot). kb-cli's `kb daemon` subcommand + the standalone `kb-server` binary
//! both call `serve_loop`; `serve(config)` is a single-shot convenience.

pub mod embed_cache;
pub mod live_registry;
pub mod live_tail_cache;
pub mod mdns;
pub mod middleware;
pub mod replay_cache;
pub mod router;
pub mod routes;
pub mod scrub;
pub mod slate_registry;
pub mod state;
pub mod touches_cache;

use anyhow::{Context, Result};
use kb_core::{
    config::KbConfig,
    embed::{models_cache_dir, Embedder},
    events::EventBus,
    paths::KbPaths,
    storage::StorageActor,
    types::KbName,
    watcher::{Watcher, WatcherConfig},
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

pub use router::build_router;
pub use state::{KbContext, KbHandles};

/// PF-B1 — process-wide build stamp, injected by the launching BIN.
///
/// The git probe lives in the leaf `kb-buildstamp` crate (the only
/// build.rs watching `.git/HEAD`), so this lib no longer recompiles on
/// every commit — and cargo cannot scope a dependency to one target of a
/// package, so the stamp must arrive at runtime rather than via a
/// bin-only dep. `kb daemon` (kb-cli) injects the real probe values; the
/// standalone `kb-server` bin stamps from plain `option_env!`. Unset
/// (tests, embedded consumers) the identity route serves the same
/// `0.0.0-dev`/`unknown` fallbacks the old build.rs produced absent git —
/// `"unknown"` is the value the SPA drift guard no-ops on by contract.
#[derive(Debug, Clone, Copy)]
pub struct BuildStamp {
    pub version: &'static str,
    pub build_sha: &'static str,
}

static BUILD_STAMP: std::sync::OnceLock<BuildStamp> = std::sync::OnceLock::new();

/// Install the process-wide build stamp. First caller wins; later calls
/// are ignored so an in-process config restart can't flip it mid-flight.
pub fn set_build_stamp(version: &'static str, build_sha: &'static str) {
    let _ = BUILD_STAMP.set(BuildStamp { version, build_sha });
}

pub(crate) fn build_stamp() -> BuildStamp {
    BUILD_STAMP.get().copied().unwrap_or(BuildStamp {
        version: "0.0.0-dev",
        build_sha: "unknown",
    })
}

/// Run the daemon to completion (or until the process is signalled).
///
/// Steps:
/// 1. Resolve XDG paths for the daemon name.
/// 2. For each kb in the config: open storage, start watcher + indexer.
/// 3. Build the axum router with all 9 v0.0.1 routes + virtual-host
///    dispatch for the `<id>.artifacts.localhost` subdomain.
/// 4. Bind the listener (default `127.0.0.1:4000`) and serve.
pub async fn serve(config: KbConfig) -> Result<()> {
    let paths = build_xdg_paths(&config)?;
    // Single-shot (no restart loop): for callers that hand us an
    // already-parsed config without a source path. Web edits go through
    // `serve_loop`, which owns the config path + reload cycle.
    let config_path = paths.config_file();
    serve_with_paths(config, config_path, paths)
        .await
        .map(|_| ())
}

/// CE — how one `serve_with_paths` run ended: the process should exit
/// (`Shutdown` — OS signal or `/api/shutdown`) or the daemon should
/// rebuild from a freshly-read config (`Restart` — `PUT /api/config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServeOutcome {
    Restart,
    Shutdown,
}

/// CE — the daemon boot loop. Serve; on a config-PUT restart, re-read the
/// config from `config_path` + rebuild, rolling back to the last-known-
/// good config if the new one fails to boot (e.g. an unbindable addr) so
/// a bad edit can't strand the daemon. `paths` is pinned for the loop's
/// lifetime — the pid file + state dir derive from the daemon name, which
/// `PUT /api/config` refuses to change. Both `kb daemon` and the
/// standalone binary call this.
pub async fn serve_loop(config_path: std::path::PathBuf, paths: KbPaths) -> Result<()> {
    let mut current = load_config_for_serve(&config_path);
    let mut last_good: Option<KbConfig> = None;
    loop {
        match serve_with_paths(current.clone(), config_path.clone(), paths.clone()).await {
            Ok(ServeOutcome::Shutdown) => return Ok(()),
            Ok(ServeOutcome::Restart) => {
                // `current` served successfully → it's the new last-good.
                last_good = Some(current.clone());
                // Re-read the edited file. If it's gone bad since the PUT
                // (parse error or a hard validation issue from external
                // tampering), keep serving last-good rather than booting
                // a broken config.
                current = match KbConfig::load(&config_path) {
                    Ok(next) if next.validate().iter().all(|i| !i.is_hard()) => {
                        tracing::info!("config changed; restarting daemon in-process");
                        next
                    }
                    Ok(_) => {
                        tracing::error!(
                            "reloaded config has hard validation errors; keeping last-good"
                        );
                        last_good.clone().expect("just set")
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "reloaded config failed to parse; keeping last-good");
                        last_good.clone().expect("just set")
                    }
                };
            }
            Err(e) => {
                // `current` failed to bind/boot. Roll back to a *different*
                // last-good; otherwise (first boot, or last-good itself
                // broken) the failure is fatal.
                match &last_good {
                    Some(good) if !configs_equal(good, &current) => {
                        tracing::error!(error = %e, "restart failed to boot; rolling back to last-good config");
                        current = good.clone();
                    }
                    _ => return Err(e),
                }
            }
        }
    }
}

/// CE — load the config at `path`, falling back to `KbConfig::default()`
/// when it's missing/unreadable (mirrors the CLI's `load_config_or_default`
/// so a daemon can boot before `kb add`). The `kb daemon` / standalone
/// callers pre-load + propagate a parse error on a malformed file before
/// reaching `serve_loop`, so this default path is for the missing-file
/// case in practice.
fn load_config_for_serve(path: &std::path::Path) -> KbConfig {
    match KbConfig::load(path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "config not loaded — using defaults");
            KbConfig::default()
        }
    }
}

/// CE — structural equality via canonical TOML render (`KbConfig` doesn't
/// derive `PartialEq`). The rollback check uses it to tell whether the
/// failed config actually differs from last-good.
fn configs_equal(a: &KbConfig, b: &KbConfig) -> bool {
    match (a.to_string_pretty(), b.to_string_pretty()) {
        (Ok(sa), Ok(sb)) => sa == sb,
        _ => false,
    }
}

/// `serve` variant taking explicit `KbPaths` + the resolved config path.
/// Tests use this with `KbPaths::rooted_at(tmpdir, daemon_name)` to avoid
/// mutating process-global `XDG_*` env vars (which race under parallel
/// `cargo test`). Returns how the run ended so `serve_loop` can decide
/// whether to rebuild or exit.
pub async fn serve_with_paths(
    config: KbConfig,
    config_path: std::path::PathBuf,
    paths: KbPaths,
) -> Result<ServeOutcome> {
    let paths = Arc::new(paths);
    paths.ensure_dirs().context("create state dirs")?;

    // CE — bind FIRST, before spawning any per-kb tasks. A bad addr (e.g.
    // an unbindable port from a config edit) then fails here, before
    // anything is spawned, so the early `Err` leaks nothing — and in
    // `serve_loop` it triggers the last-good rollback.
    let addr = config.server.addr.clone();
    let listener = bind_with_retry(&addr).await?;
    let local_addr = listener.local_addr()?;

    // Reliability — when serving on the IPv4 loopback (the default
    // `127.0.0.1` bind), ALSO bind the IPv6 loopback `[::1]` on the same
    // port. On hosts where `localhost` resolves to `::1` first (a common
    // /etc/hosts or gai.conf ordering quirk), an IPv4-only bind leaves the
    // browser hitting a dead address — it then falls back to a stale
    // cached page, which looks like "the app won't update". Binding both
    // loopback families makes `localhost:<port>` and the
    // `*.artifacts.localhost` iframes resolve regardless of which family
    // the OS prefers, WITHOUT the LAN exposure of a `[::]` dual-stack bind.
    // Bound here, before any task spawns, so it shares invariant #13's
    // bind-before-spawn guarantee. Best-effort: a host with IPv6 disabled
    // (or a transient TIME_WAIT rebind clash) simply keeps the IPv4
    // listener. Skipped when the primary bind is already IPv6 or
    // non-loopback (an explicit operator choice we don't second-guess).
    let listener6 = if local_addr.is_ipv4() && local_addr.ip().is_loopback() {
        let addr6 = format!("[::1]:{}", local_addr.port());
        match bind_with_retry(&addr6).await {
            Ok(l) => {
                tracing::info!(addr = %addr6, "bound IPv6 loopback companion listener");
                Some(l)
            }
            Err(e) => {
                tracing::warn!(
                    addr = %addr6, error = %e,
                    "could not bind IPv6 loopback companion; serving IPv4 loopback only"
                );
                None
            }
        }
    } else {
        None
    };

    // deep-review X2 LOW — the annotator origin gate + the artifact
    // `frame-ancestors` CSP both key off a configured `[server] parent_origin`;
    // left at the dev default (or empty) the postMessage targetOrigin stays
    // `'*'` and no CSP is sent (see `routes::artifact::postmessage_target`).
    // That's fine on a loopback bind, but on a public bind it silently disables
    // both — any page could iframe an artifact and drive its annotator. Warn so
    // the misconfig isn't invisible.
    {
        let p = config.server.parent_origin.as_str();
        let unconfigured =
            p.is_empty() || p == kb_core::config::ServerSection::DEFAULT_PARENT_ORIGIN;
        if !local_addr.ip().is_loopback() && unconfigured {
            tracing::warn!(
                addr = %local_addr,
                "serving on a non-loopback address with no [server] parent_origin set — \
                 artifact framing is unrestricted and the annotator origin gate is disabled; \
                 set parent_origin to your SPA's origin to lock both down"
            );
        }
    }

    let started_at = chrono::Utc::now();
    let spa_dist = routes::spa::resolve_spa_dist();
    // v0.4 A2 — load the bearer-token file (None when absent → auth
    // disabled, the v0.3 personal-mode default).
    let mut auth = crate::state::AuthConfig::load(&paths.token_file())
        .with_context(|| format!("read {}", paths.token_file().display()))?;
    if auth.token.is_some() {
        tracing::info!(
            path = %paths.token_file().display(),
            "loaded bearer-token auth config"
        );
    }
    // v0.34 Y1 — multi-user token registry + identity ladder config.
    auth.tokens = crate::state::AuthConfig::load_tokens_registry(&paths.tokens_file());
    auth.operator = config.identity.operator.clone();
    auth.identity_header = config.identity.header.to_ascii_lowercase();
    // Security (deep-review P1 → P0 for productization): FAIL CLOSED on a
    // token-less public bind. A non-loopback listener with no bearer token
    // would otherwise expose every `/api/*` verb — including `DELETE /api/kb`
    // — to the whole network with no authentication. Refuse to start unless
    // the operator either configures a token or explicitly acknowledges that
    // an upstream proxy is the auth gate (`KB_ALLOW_NO_AUTH=1`). A `0.0.0.0`
    // bind reports a non-loopback `local_addr`, so this catches the common
    // "bound everything, forgot the token" mistake before the socket is live.
    // In `serve_loop` a first-boot `Err` here is fatal (no last-good to roll
    // back to); a bad *reload* rolls back to the last-good config.
    // v0.34 Y1: a non-empty multi-user registry also counts as "has auth".
    if crate::middleware::refuse_public_bind_without_auth(
        local_addr.ip().is_loopback(),
        auth.has_auth(),
        crate::middleware::allow_no_auth(),
    ) {
        anyhow::bail!(
            "refusing to serve on non-loopback address {local_addr} without a bearer token — \
             a token-less public bind would expose every /api verb (including DELETE /api/kb) \
             unauthenticated. Configure a token at {} (or a tokens registry at {}), then restart, \
             or set KB_ALLOW_NO_AUTH=1 to acknowledge that an upstream proxy provides authentication.",
            paths.token_file().display(),
            paths.tokens_file().display()
        );
    }
    // v0.7.1 C1 — resolve the trusted-proxy allowlist once and share the
    // Arc across the auth, rate-limit, and outbound-scrub layers so they
    // evaluate the same trusted set.
    let trusted_proxies = Arc::new(crate::state::parse_trusted_proxies(
        &config.server.trusted_proxies,
    ));
    if !trusted_proxies.is_empty() {
        tracing::info!(count = trusted_proxies.len(), "loaded trusted_proxies");
    }
    auth.trusted_proxies = trusted_proxies.clone();
    let rate_limits = crate::state::RateLimits::from_section(config.server.rate_limit.as_ref());
    let origin = crate::state::OriginConfig {
        artifact_host_suffix: config.server.artifact_host_suffix.clone(),
        parent_origin: config.server.parent_origin.clone(),
        trusted_proxies: trusted_proxies.clone(),
    };
    let mut handles = KbHandles::new(paths.daemon_name.clone(), paths.clone(), started_at)
        .with_ui(config.ui.clone())
        .with_spa_dist(spa_dist)
        .with_auth(auth)
        .with_rate_limits(rate_limits)
        .with_origin(origin)
        .with_share(config.share.clone())
        // TM-track — resolve the detailed-metrics flag + pre-seed the per-kb
        // request map. MUST precede the kb loop: `handles.pipeline` is the Arc
        // threaded into each kb's storage actor + indexer below.
        .with_metrics(
            config.server.metrics,
            config.kb.keys().map(|k| k.as_str().to_string()),
        )
        // CE — install the running config + the file it came from so
        // GET/PUT /api/config can read it + write edits back to the right
        // file (explicit --config or the home-local default).
        .with_config(config.clone(), config_path);

    // CE — background task handles, joined on shutdown/reload before the
    // kbs' storage + embedders drop so a reload doesn't leak them.
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // Track: one shared kb-embedder subprocess per distinct resolved model
    // (not per kb) — kbs on the same model clone one Arc<Mutex<Embedder>>.
    // Lives for the whole serve; on shutdown it + the KbContexts drop, and the
    // last Arc kills the subprocess (after teardown_tasks joins the indexers).
    // S3 — install the daemon-global corpus mount table so the indexer's
    // session-capture hook can resolve a session's touched-file paths to the
    // artifact they edited in ANY corpus (the cross-corpus link, A4/A7).
    install_corpus_mounts(&config);
    // W3.A — install the `[projects.*]` registry beside it (same lifecycle:
    // fresh install here, re-install on a config-reload restart, #13).
    install_project_registry(&config);

    let mut embedders: std::collections::HashMap<String, Arc<Mutex<Embedder>>> =
        std::collections::HashMap::new();
    for (kb_name, kb_section) in &config.kb {
        let resolved_model = config.resolved_embedding_model(kb_section);
        if let Some(name) = kb_section.embedding_model.as_deref() {
            if kb_core::embed::model_info(name).is_none() {
                tracing::warn!(
                    kb = %kb_name,
                    model = name,
                    "[kb.{kb_name}] embedding_model is not in SUPPORTED_MODELS; \
                     falling back to [defaults] / registry default"
                );
            }
        }
        // X1 — resolve the extension map here (full `KbConfig` in scope), like
        // `resolved_model`; `bring_up_kb` installs it on the sink + indexer.
        let ext_map = config.resolved_extension_map(kb_section);
        let brought_up = bring_up_kb(
            kb_name,
            kb_section,
            resolved_model,
            &mut embedders,
            &config.indexer,
            &config.storage,
            &paths,
            &config.server.artifact_host_suffix,
            handles.bus.clone(),
            handles.pipeline.clone(),
            handles.shutdown.subscribe(),
            handles.review_lock_for(kb_name),
            ext_map,
        )
        .await
        .with_context(|| format!("bring up kb {kb_name}"));
        let (context, kb_tasks) = match brought_up {
            Ok(v) => v,
            Err(e) => {
                // G3 — a later kb failed to boot. The already-spawned tasks
                // for earlier kbs (watchers / indexers / reconcilers) must be
                // aborted + joined before we return: a bare `?` would only
                // DROP their JoinHandles (tokio drop detaches, doesn't abort),
                // leaking them plus the shared-embedder subprocess clones they
                // hold — and the indexer's EventBus-sender clone keeps the bus
                // open (the reference cycle invariant #13 calls out). Aborting
                // releases those clones; returning then drops the inserted
                // KbContexts so the storage actors + embedder subprocess
                // terminate cleanly. (Bind happened before any spawn, so the
                // listener isn't leaked either.)
                for t in &tasks {
                    t.abort();
                }
                for t in tasks {
                    let _ = t.await;
                }
                return Err(e);
            }
        };
        tasks.extend(kb_tasks);
        handles.insert(kb_name.clone(), context);
    }

    // v0.34 Y1 — marker-gated identity backfill (empty-user history +
    // legacy list overrides → operator). Idempotent; every boot is fine.
    {
        let operator = handles.operator_user().to_string();
        let now_unix = chrono::Utc::now().timestamp();
        for (kb_name, ctx) in handles.kbs.iter() {
            match ctx
                .storage
                .identity_backfill(operator.clone(), now_unix)
                .await
            {
                Ok(n) if n > 0 => {
                    tracing::info!(
                        kb = %kb_name,
                        rows = n,
                        operator = %operator,
                        "identity backfill stamped operator on pre-multi-user rows"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "identity_backfill failed");
                }
            }
        }
    }

    let handles = Arc::new(handles);
    // N7: spawn the request-rate ticker. Snapshots the atomic counter
    // every second, computes the delta, and emits a `metrics.tick`
    // SSE event with `requests_total` and `requests_last_sec`. Cheap
    // (1Hz, single Relaxed load) and the SSE bus is broadcast so
    // every connected TUI gets it.
    tasks.push(spawn_metrics_ticker(handles.clone()));
    // X1 — daemon-wide reactive event→webhook bridge. Spawns nothing when
    // `[webhooks]` is unset; when set, one subscriber POSTs selected events
    // to the configured URL. Joined in `teardown_tasks` like every task here.
    if let Some(h) = spawn_webhook_bridge(
        handles.bus.clone(),
        handles.shutdown.subscribe(),
        config.webhooks.clone(),
    ) {
        tasks.push(h);
    }
    // R3 — daemon-wide opt-in retention prune. Spawns NOTHING unless
    // `[retention]` sets at least one window; when set, one daily task (also
    // running once at boot) deletes history + reading_sections rows older than
    // the window across every kb. Joined in `teardown_tasks` like every task
    // here, so an in-process config reload can't leak it.
    if let Some(h) = spawn_retention_prune(handles.clone(), &config.retention) {
        tasks.push(h);
    }
    // L2 — daemon log-file retention. Always on (validated ≥ 1 day, default
    // 14): one daily task (first tick at boot) deletes aged
    // `<state>/log/kb.ndjson.*` files. Joined in `teardown_tasks` like the
    // R3 prune above.
    tasks.push(spawn_log_retention_prune(
        handles.clone(),
        config.server.log_retention_days,
    ));
    // One-shot startup auto-compact for kbs whose lance dataset has
    // shredded into many small fragments / many manifest versions. The
    // helper inspects each kb's dataset shape via `dataset_stats()`
    // and spawns a background `compact_all()` per kb that crosses the
    // heuristic. Per-kb so a single fragmented dataset doesn't delay
    // the rest from coming up; the storage actor's single-writer queue
    // already serialises the compact against any pending writes.
    let compact_tasks = spawn_startup_compact(handles.clone());
    // Captured before `build_router` consumes `handles`: `shutdown_signal`
    // flips this on the OS signal, which both ends every `/api/events` SSE
    // stream and starts the bounded-drain clock below.
    let shutdown_tx = handles.shutdown.clone();
    // CE — outcome flags read after the drain. `terminate` (OS signal)
    // wins over `restart_requested` (a config PUT) so a SIGTERM mid-restart
    // still stops the daemon.
    let terminate = handles.terminate.clone();
    let restart_requested = handles.restart_requested.clone();
    // Captured before `build_router` consumes `handles`: the daemon-wide
    // query-embed cache, persisted on teardown so hot queries survive both the
    // OS-signal shutdown and the CE in-process restart (the loop re-enters
    // `serve_with_paths` → `KbHandles::new` → `load_or_new` reloads it).
    let embed_cache = handles.embed_cache.clone();
    let app = build_router(handles);
    // listener + local_addr were bound up-front (before task spawns).
    tracing::info!(addr = %local_addr, daemon = %paths.daemon_name, "kb-server listening");

    // v0.4 C1 — opt-in mDNS advertise. The returned ServiceDaemon
    // must outlive the server task so the multicast registration
    // stays alive; `_mdns` parks it in scope until `axum::serve`
    // returns.
    let _mdns = if config.server.mdns {
        match crate::mdns::advertise(&paths.daemon_name, local_addr) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::warn!(error = %e, "mdns advertise failed; continuing without it");
                None
            }
        }
    } else {
        None
    };

    // v0.4 A2 — `into_make_service_with_connect_info` wires the
    // `ConnectInfo<SocketAddr>` extractor that auth_bearer uses for
    // loopback bypass detection.
    //
    // Deep-review LOW (graceful shutdown): listen for SIGTERM /
    // SIGINT and stop accepting new connections, then await axum's
    // graceful drain. Once axum returns, the storage actor + watcher
    // get dropped — the actor's mpsc receive loop ends cleanly and
    // any in-flight `handle.foo().await` callers see their oneshot
    // dropped (returning Err), which is exactly the behaviour callers
    // already cope with on shutdown. Without this hook, SIGTERM
    // aborted axum mid-request and any inflight HTTP handler that had
    // already 200'd but not yet flushed the storage message could
    // lose data.
    let server = axum::serve(
        listener,
        app.clone()
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(shutdown_tx.clone(), terminate.clone()));

    // Reliability — serve the SAME router on the IPv6 loopback companion
    // (bound up-front above). A fresh `shutdown_signal` future + the same
    // shutdown watch means both listeners drain together on stop/reload.
    let server6 = listener6.map(|l6| {
        axum::serve(
            l6,
            app.clone()
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal(shutdown_tx.clone(), terminate.clone()))
    });

    // Bound the graceful drain. SSE streams close on the shutdown watch, so
    // a normal drain finishes promptly; the deadline is the backstop that
    // guarantees the process exits inside the operator's `kb daemon stop`
    // window even if some connection ignores the close. Both listeners are
    // awaited together — `try_join!` short-circuits if either serve loop
    // errors, and both observe the same drain deadline.
    run_with_drain_deadline(
        async move {
            match server6 {
                Some(s6) => {
                    tokio::try_join!(
                        async { server.await.context("axum::serve (IPv4 loopback)") },
                        async { s6.await.context("axum::serve (IPv6 loopback)") },
                    )?;
                    Ok(())
                }
                None => server.await.context("axum::serve"),
            }
        },
        shutdown_tx.subscribe(),
        Duration::from_secs(GRACEFUL_DRAIN_SECS),
    )
    .await?;

    // CE — the shutdown watch has fired; cancel + join the background
    // tasks so their storage/embedder/bus clones drop. On the current
    // single-shot path this is just clean teardown; CE4's reload loop
    // depends on it so a restart doesn't leak tasks or kb-embedder
    // subprocesses. Startup-compact passes are aborted (transactional —
    // an interrupted compaction just doesn't finish); the cancellable
    // loops get a bounded join (they observe the same shutdown watch).
    teardown_tasks(tasks, compact_tasks).await;

    // Persist the query-embed cache AFTER the drain (in-flight searches have
    // finished, so the snapshot is final). Best-effort — a save failure is
    // logged, never fatal, and never blocks the outcome.
    if let Err(e) = embed_cache.save(&paths.embed_cache_file()) {
        tracing::warn!(error = %e, "failed to persist query-embed cache");
    }

    // CE — decide the outcome. terminate (real stop) wins; else a config
    // PUT asked for a restart; else a plain shutdown (e.g. /api/shutdown).
    use std::sync::atomic::Ordering;
    let outcome = if terminate.load(Ordering::SeqCst) {
        ServeOutcome::Shutdown
    } else if restart_requested.load(Ordering::SeqCst) {
        ServeOutcome::Restart
    } else {
        ServeOutcome::Shutdown
    };
    Ok(outcome)
}

/// CE — bind with a short bounded retry. After an in-process restart the
/// previous listener has just dropped; tokio sets SO_REUSEADDR so a
/// same-addr rebind normally succeeds at once, but a connection lingering
/// in TIME_WAIT can briefly hold the port. Retry a few times before
/// giving up (which, in `serve_loop`, triggers last-good rollback).
async fn bind_with_retry(addr: &str) -> Result<TcpListener> {
    let mut last_err = None;
    for attempt in 0..5 {
        match TcpListener::bind(addr).await {
            Ok(l) => return Ok(l),
            Err(e) => {
                tracing::warn!(addr, attempt, error = %e, "bind failed; retrying in 200ms");
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    Err(last_err.expect("loop ran ≥1 time")).with_context(|| format!("bind {addr} (after retries)"))
}

/// CE — cancel one-shot compaction tasks + bounded-join the cancellable
/// background tasks after the shutdown watch has fired. Total wait is
/// bounded by `GRACEFUL_DRAIN_SECS` across all handles (each task already
/// observed the shutdown signal, so this is a backstop, not the norm).
async fn teardown_tasks(
    tasks: Vec<tokio::task::JoinHandle<()>>,
    compact_tasks: Vec<tokio::task::JoinHandle<()>>,
) {
    for t in &compact_tasks {
        t.abort();
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(GRACEFUL_DRAIN_SECS);
    for t in tasks.into_iter().chain(compact_tasks) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let _ = tokio::time::timeout(remaining, t).await;
    }
}

/// Maximum time the graceful drain may run after the shutdown signal fires
/// before `serve` returns anyway (dropping the server future, which aborts
/// any still-open connections). Comfortably inside `kb daemon stop`'s 10 s
/// SIGTERM wait so a clean stop never escalates to SIGKILL.
const GRACEFUL_DRAIN_SECS: u64 = 5;

/// Run `server` to completion, but once `shutdown` flips to `true` give the
/// graceful drain at most `grace` before returning regardless. The deadline
/// clock starts when the signal fires (not when serving starts), so a
/// long-running daemon is unaffected until shutdown. Returns the server's
/// own result if it finishes first; `Ok(())` if the deadline wins.
async fn run_with_drain_deadline<F>(
    server: F,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    grace: Duration,
) -> Result<()>
where
    F: std::future::Future<Output = Result<()>>,
{
    tokio::pin!(server);
    tokio::select! {
        res = &mut server => res,
        _ = async move {
            // Resolves when the signal fires (or every sender is dropped),
            // then waits out the grace period.
            let _ = shutdown.wait_for(|&down| down).await;
            tokio::time::sleep(grace).await;
        } => {
            tracing::warn!(
                grace_secs = grace.as_secs(),
                "graceful drain exceeded deadline; forcing shutdown \
                 (a connection ignored the close — likely a long-lived SSE client)"
            );
            Ok(())
        }
    }
}

/// N7+N8: 1Hz metrics ticker. Reads the atomic request counter +
/// storage-channel depth snapshots and broadcasts a `metrics.tick`
/// SSE event so consumers (the TUI's TRAFFIC tab) can plot a real
/// req/sec metric and a backpressure gauge.
fn spawn_metrics_ticker(handles: Arc<crate::state::KbHandles>) -> tokio::task::JoinHandle<()> {
    use std::sync::atomic::Ordering;
    use tokio::time::{interval, Duration};
    // CE — exit on shutdown so an in-process reload doesn't leak the
    // ticker (which would double the `metrics.tick` rate each restart).
    let mut shutdown = handles.shutdown.subscribe();
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(1));
        // Skip the first immediate tick — the delta would be 0 anyway,
        // and avoiding it keeps the very first SSE consumer from
        // seeing a "tick fired before any traffic" event.
        ticker.tick().await;
        let mut prev_total: u64 = 0;
        loop {
            tokio::select! {
                _ = shutdown.wait_for(|&down| down) => break,
                _ = ticker.tick() => {}
            }
            let curr_total = handles.metrics.total.load(Ordering::Relaxed);
            let last_sec = curr_total.saturating_sub(prev_total);
            prev_total = curr_total;
            // N8: take a max-over-kbs sample of the storage channel
            // depth. Each kb has its own storage actor (per the
            // single-writer invariant); summing would obscure which
            // is back-pressured. Max highlights the worst case — the
            // most useful signal for the operator.
            let queue_depth: u32 = handles
                .kbs
                .values()
                .map(|ctx| ctx.storage.queue_depth() as u32)
                .max()
                .unwrap_or(0);
            handles
                .metrics
                .storage_channel_depth
                .store(queue_depth, Ordering::Relaxed);
            // P3+P4: per-route snapshot. Each route's cumulative
            // count + p50/p95 from its bucket histogram. Stable shape
            // (8 entries always) so the TUI's table layout doesn't
            // shift between ticks.
            let routes: Vec<serde_json::Value> = crate::state::RouteKind::ALL
                .iter()
                .map(|k| {
                    let m = &handles.metrics.by_route[*k as usize];
                    let count = m.count();
                    let buckets = m.buckets_snapshot();
                    let p50 = crate::state::percentile_ms(&buckets, 0.5);
                    let p95 = crate::state::percentile_ms(&buckets, 0.95);
                    serde_json::json!({
                        "kind": k.label(),
                        "count": count,
                        "p50_ms": p50,
                        "p95_ms": p95,
                    })
                })
                .collect();
            // v0.16 Q-track — proactive embedder liveness. Probe each
            // DISTINCT embedder subprocess (kbs sharing a model share one)
            // off the async runtime: a cheap `try_wait` notices an idle
            // death (e.g. an OOM-kill between requests) the request path
            // wouldn't, and recovers it now so search doesn't silently
            // degrade to keyword-only on the user's request. `spawn_blocking`
            // keeps the (rare) ONNX-reloading respawn off the runtime; the
            // probe takes the embedder's own `std::sync::Mutex`, so it can't
            // race a concurrent mid-request respawn. Aggregated daemon-wide:
            // `degraded` iff any embedder is currently unrecoverable,
            // `respawn_count` is the cumulative crash count (a climbing
            // value is the "embedder keeps dying" signal).
            let mut embedder_degraded = false;
            let mut embedder_respawn_count: u64 = 0;
            for emb in handles.unique_embedders() {
                let (alive, respawns) = tokio::task::spawn_blocking(move || {
                    let mut guard = emb.lock().unwrap_or_else(|e| e.into_inner());
                    (guard.ensure_alive(), guard.respawn_count())
                })
                .await
                .unwrap_or((true, 0)); // a panicked probe must not false-degrade
                if !alive {
                    embedder_degraded = true;
                }
                embedder_respawn_count = embedder_respawn_count.saturating_add(respawns);
            }
            // v0.24 T1 — mirror the probe result into the shared metrics
            // struct so `GET /api/metrics` reports embedder health without
            // an SSE subscription (kb metrics / kb fleet status read it).
            handles
                .metrics
                .embedder_degraded
                .store(embedder_degraded, Ordering::Relaxed);
            handles
                .metrics
                .embedder_respawn_count
                .store(embedder_respawn_count, Ordering::Relaxed);
            handles.bus.emit(
                "metrics.tick",
                serde_json::json!({
                    "requests_total": curr_total,
                    "requests_last_sec": last_sec,
                    // SW1 — live `/api/events` consumers (SPA tabs or the
                    // shared worker, kb-cli watchers). Additive field;
                    // SPA parsers ignore unknown keys.
                    "sse_subscribers":
                        handles.metrics.sse_clients.load(Ordering::Relaxed),
                    "storage_channel_depth": queue_depth,
                    "storage_channel_capacity":
                        kb_core::storage::actor::StorageHandle::queue_capacity() as u32,
                    "routes": routes,
                    "embedder_degraded": embedder_degraded,
                    "embedder_respawn_count": embedder_respawn_count,
                }),
            );
        }
    })
}

/// X1 — daemon-wide event→webhook bridge. One task subscribes to the
/// firehose and POSTs every envelope whose `type` is in `types` to `url`.
/// This is the smallest proof that the event bus is a plugin substrate: a
/// direct clone of the per-kb history-ring subscriber (`bus.subscribe()` →
/// filter → act), but the action is an outbound POST instead of a ring push.
///
/// Read-only + post-emit, so it respects every security invariant — it adds
/// no inbound surface and never writes storage. Returns `None` when no `url`
/// / no `types` are configured, so an unconfigured daemon spawns nothing.
///
/// Like every other background task it watches the shutdown watch and is
/// joined in `teardown_tasks`, so an in-process config reload (CE) can't leak
/// it or double up POSTs. The POST is `await`ed (bounded by `timeout_ms`)
/// only to log the outcome; a slow endpoint makes this subscriber lag and
/// drop events (logged) — never daemon backpressure (the bounded-bus
/// invariant). Webhook consumers must therefore tolerate gaps, exactly like
/// any `/api/events` subscriber.
fn spawn_webhook_bridge(
    bus: Arc<EventBus>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    webhooks: Option<kb_core::config::WebhooksSection>,
) -> Option<tokio::task::JoinHandle<()>> {
    let webhooks = webhooks?;
    let url = webhooks.url.trim().to_string();
    if url.is_empty() || webhooks.types.is_empty() {
        return None;
    }
    let allow_private = webhooks.allow_private;
    // Fail closed at spawn on syntax / IP-literal policy so a bad config
    // never starts a subscriber. Hostname resolution + the connect-IP pin
    // happen per-POST in `prepare_webhook_dial` (off the runtime), so no
    // blocking DNS runs here or on the config-validate path.
    if let Err(msg) = kb_core::webhook_url::validate_webhook_url(&url, allow_private) {
        tracing::warn!(url = %url, error = %msg, "webhook bridge: url refused; bridge disabled");
        return None;
    }
    let types: std::collections::HashSet<String> = webhooks.types.into_iter().collect();
    let timeout = Duration::from_millis(webhooks.timeout_ms.unwrap_or(5000));
    tracing::info!(url = %url, types = ?types, allow_private, "event→webhook bridge enabled");
    let mut rx = bus.subscribe();
    Some(tokio::spawn(async move {
        loop {
            // The POST is deliberately AWAITED outside the `select!` arm: a
            // `select!` arm body that awaits would keep the macro's `Out`
            // enum (which can hold the `watch::Ref` from `wait_for`, a
            // `!Send` guard) alive across the await, making the task future
            // un-spawnable. So the select only yields the next envelope.
            let env = tokio::select! {
                _ = shutdown.wait_for(|&d| d) => break,
                r = rx.recv() => match r {
                    Ok(env) => env,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "webhook bridge lagged; dropped events (slow endpoint)");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            };
            if !types.contains(&env.type_) {
                continue;
            }
            // Resolve + policy-filter *then* pin those addrs on the client so
            // reqwest cannot re-resolve to a blocked IP (DNS rebinding TOCTOU).
            // No redirects (below): a 302 must not retarget past the pin.
            // `getaddrinfo` is blocking, so resolve OFF the runtime.
            let dial = {
                let url = url.clone();
                tokio::task::spawn_blocking(move || {
                    kb_core::webhook_url::prepare_webhook_dial(&url, allow_private)
                })
                .await
            };
            let dial = match dial {
                Ok(Ok(d)) => d,
                Ok(Err(msg)) => {
                    tracing::warn!(
                        url = %url, error = %msg, event = %env.type_,
                        "webhook POST skipped (url policy)",
                    );
                    continue;
                }
                Err(join_err) => {
                    tracing::warn!(
                        url = %url, error = %join_err, event = %env.type_,
                        "webhook POST skipped (resolve task failed)",
                    );
                    continue;
                }
            };
            let client = match reqwest::Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::none())
                .resolve_to_addrs(&dial.domain, &dial.addrs)
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        url = %url, error = %e, event = %env.type_,
                        "webhook POST skipped (client build)",
                    );
                    continue;
                }
            };
            match client.post(&dial.url).json(&env).send().await {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => tracing::warn!(
                    url = %url, status = %resp.status(), event = %env.type_,
                    "webhook POST returned non-2xx",
                ),
                Err(e) => tracing::warn!(
                    url = %url, error = %e, event = %env.type_,
                    "webhook POST failed",
                ),
            }
        }
    }))
}

/// Fragments-per-row above which we consider the dataset shredded
/// enough to compact at startup. Lance creates one fragment per
/// `merge_insert` (one per indexed file), so a healthy reindexed kb
/// sits at ~1 fragment/row; we tolerate up to 8 before kicking off
/// maintenance. Threshold picked empirically — the `platform` kb at
/// 397 rows / 2 592 fragments (6.5×) had degraded into 5–10 s hybrid
/// queries, so the bar wants to land below that ratio.
const COMPACT_FRAGMENTS_PER_ROW: u64 = 8;
/// Hard cap on the version count alone: a long-running daemon that
/// barely writes still accumulates manifest versions over time, and
/// past a few hundred the per-query overhead of consulting them adds
/// up even when the fragment ratio looks fine. 200 matches the
/// observed sweet spot from `docs/spike-findings.md` benchmarks
/// (lance's own default prune window leaves more than this only when
/// writes outpace cleanup, which is exactly the case we want to
/// catch).
const COMPACT_MAX_VERSIONS: u64 = 200;

/// Absolute small-fragment count that warrants compaction, honoured in
/// BOTH the startup and periodic paths. Each `merge_insert` upsert
/// appends one small data fragment and compaction merges them away, so
/// this signal is SELF-RESOLVING — it drops to ~0 after a pass and won't
/// re-fire — and it's the one that actually tracked the latency rot we
/// measured (58 small fragments → 4× hybrid latency on a 91-row kb). The
/// fragments-per-ROW ratio below misses that entirely: a 91-row kb would
/// need 728 fragments to reach 8×. 32 sits well under the 58 that hurt
/// and well over a healthy steady state of a handful. Tunable.
const COMPACT_SMALL_FRAGMENTS: u64 = 32;

/// SC1 — fallback cadence for the periodic auto-compact check when the
/// reconcile pass is disabled (`reconcile_secs = 0`). Pre-SC1 the check
/// rode the reconcile loop, so disabling reconcile silently disabled
/// periodic compaction too. 300 s matches the check's spirit (cheap
/// manifest reads, self-throttling optimize) without re-introducing a
/// coupling knob.
const COMPACT_CHECK_SECS_DEFAULT: u64 = 300;

/// SC1 — the periodic auto-compact ticker's cadence: follows
/// `reconcile_secs` when the reconcile pass is on (the pre-SC1 rhythm),
/// else [`COMPACT_CHECK_SECS_DEFAULT`]. Never zero — the compact loop is
/// always spawned.
fn compact_check_interval_secs(reconcile_secs: u64) -> u64 {
    if reconcile_secs > 0 {
        reconcile_secs
    } else {
        COMPACT_CHECK_SECS_DEFAULT
    }
}

/// Decide whether a dataset warrants compaction.
///
/// `small_fragments` is the steady-state, self-resolving trigger honoured
/// in both modes (see [`COMPACT_SMALL_FRAGMENTS`]).
///
/// The startup path (`periodic == false`) additionally honours the
/// fragments-per-row ratio (pathologically fragmented huge corpora) and
/// the absolute version cap (manifest pile-up) — both safe at startup: it
/// runs once, and versions left from previous days are outside lance's
/// retention window so a prune actually removes them.
///
/// The periodic path (`periodic == true`) deliberately does NOT honour
/// the version cap: inside lance's retention window those versions can't
/// be pruned, so a version-cap trigger would re-fire on every reconcile
/// tick forever (a compaction storm) while never reducing the count.
/// `rows == 0` short-circuits the ratio term so a barely-indexed kb
/// doesn't trip on a tiny absolute fragment count.
fn wants_compaction(
    rows: u64,
    fragments: u64,
    small_fragments: u64,
    versions: u64,
    periodic: bool,
) -> bool {
    if small_fragments >= COMPACT_SMALL_FRAGMENTS {
        return true;
    }
    if periodic {
        return false;
    }
    let frag_per_row = if rows == 0 {
        0
    } else {
        fragments / rows.max(1)
    };
    frag_per_row > COMPACT_FRAGMENTS_PER_ROW || versions > COMPACT_MAX_VERSIONS
}

/// Inspect one kb's dataset and, if [`needs_compaction`], run
/// `compact_all` through the storage actor. Shared by the startup pass
/// and the periodic reconcile loop; `trigger` (`"startup"` | `"periodic"`)
/// tags the log lines + `maintenance.compact.*` SSE envelopes.
///
/// Reads route through the same per-kb actor, so a compaction pass
/// briefly blocks search for this kb while it runs. That's an acceptable
/// trade because the heuristic only fires when the dataset is genuinely
/// fragmented, and a compaction resets fragments + versions — so it
/// self-throttles to roughly once per `COMPACT_MAX_VERSIONS` writes, and
/// the alternative (no periodic pass) is permanent latency creep on a
/// daemon that never restarts.
async fn maybe_compact_kb(
    kb: &str,
    storage: &kb_core::storage::StorageHandle,
    bus: &kb_core::events::EventBus,
    periodic: bool,
) {
    let trigger = if periodic { "periodic" } else { "startup" };
    let stats = match storage.dataset_stats().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(kb = %kb, trigger, error = %e, "auto-compact: dataset_stats failed");
            return;
        }
    };
    if !wants_compaction(
        stats.rows,
        stats.fragments,
        stats.small_fragments,
        stats.versions,
        periodic,
    ) {
        tracing::debug!(
            kb = %kb, trigger,
            rows = stats.rows, fragments = stats.fragments,
            small_fragments = stats.small_fragments, versions = stats.versions,
            "auto-compact: dataset healthy, skipping"
        );
        return;
    }
    tracing::info!(
        kb = %kb, trigger,
        rows = stats.rows, fragments = stats.fragments,
        small_fragments = stats.small_fragments, versions = stats.versions,
        "auto-compact: dataset fragmented, optimizing"
    );
    bus.emit(
        "maintenance.compact.started",
        serde_json::json!({
            "kb": kb,
            "trigger": trigger,
            "rows": stats.rows,
            "fragments": stats.fragments,
            "small_fragments": stats.small_fragments,
            "versions": stats.versions,
        }),
    );
    let started = std::time::Instant::now();
    match storage.compact_all().await {
        Ok(out) => {
            let ms = started.elapsed().as_millis() as u64;
            tracing::info!(
                kb = %kb, trigger, ms,
                fragments_removed = out.fragments_removed,
                fragments_added = out.fragments_added,
                old_versions_removed = out.old_versions_removed,
                "auto-compact: done"
            );
            bus.emit(
                "maintenance.compact.done",
                serde_json::json!({
                    "kb": kb,
                    "trigger": trigger,
                    "ms": ms,
                    "stats": out,
                }),
            );
        }
        Err(e) => {
            tracing::warn!(kb = %kb, trigger, error = %e, "auto-compact failed");
            bus.emit(
                "maintenance.compact.failed",
                serde_json::json!({
                    "kb": kb,
                    "trigger": trigger,
                    "error": e.to_string(),
                }),
            );
        }
    }
}

/// One-shot startup maintenance — per kb, inspect dataset shape and
/// spawn a background `compact_all` when fragmentation or version
/// pile-up cross the thresholds. Bus emits `maintenance.compact.*`
/// envelopes so the TUI EVENTS tab + any `/events?types=maintenance.*`
/// subscriber sees the pass land.
fn spawn_startup_compact(
    handles: Arc<crate::state::KbHandles>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut tasks = Vec::new();
    for (kb_name, ctx) in handles.kbs.iter() {
        let bus = ctx.bus.clone();
        let storage = ctx.storage.clone();
        let kb = kb_name.clone();
        // CE — collected + aborted on teardown so a reload doesn't leave a
        // background compaction holding a storage handle (lance compaction
        // is transactional; an abort just doesn't finish that pass).
        tasks.push(tokio::spawn(async move {
            maybe_compact_kb(kb.as_str(), &storage, &bus, false).await;
        }));
    }
    tasks
}

/// R3 — resolve the `(history, reading_sections)` retention windows in
/// seconds. Returns `None` when the feature is OFF (no window set), so the
/// caller spawns NOTHING. Pure + handle-free so the spawn decision — "no
/// idle task when the feature is off" — is unit-testable without a daemon.
fn retention_windows(
    retention: &kb_core::config::RetentionSection,
) -> Option<(Option<i64>, Option<i64>)> {
    let history = retention.history_max_age_secs();
    let reading = retention.reading_sections_max_age_secs();
    (history.is_some() || reading.is_some()).then_some((history, reading))
}

/// R3 — daemon-wide opt-in retention prune. One background task that wakes
/// once at boot and then daily, and for every kb deletes `history` +
/// `reading_sections` rows older than the configured day windows. Returns
/// `None` (spawning nothing) when `[retention]` sets no window, so an
/// unconfigured daemon pays nothing.
///
/// Watches the shutdown watch + is joined in `teardown_tasks`, so an
/// in-process config reload (CE) can't leak it. History pruning is
/// invariant #8's documented retention exception — a delete of OLD rows,
/// never an edit; the task does NOT touch the R2-cascade tables (those are
/// pruned per-artifact on delete, not by age).
fn spawn_retention_prune(
    handles: Arc<crate::state::KbHandles>,
    retention: &kb_core::config::RetentionSection,
) -> Option<tokio::task::JoinHandle<()>> {
    use tokio::time::{interval, Duration};
    let (history_secs, reading_secs) = retention_windows(retention)?;
    let period = Duration::from_secs(kb_core::config::RetentionSection::PRUNE_INTERVAL_SECS);
    let mut shutdown = handles.shutdown.subscribe();
    tracing::info!(
        history_days = ?retention.history_days,
        reading_sections_days = ?retention.reading_sections_days,
        "retention prune enabled"
    );
    Some(tokio::spawn(async move {
        // `interval` fires its first tick immediately → one prune at boot,
        // then once per period.
        let mut ticker = interval(period);
        loop {
            tokio::select! {
                _ = shutdown.wait_for(|&down| down) => break,
                _ = ticker.tick() => {}
            }
            let now = chrono::Utc::now().timestamp();
            for (kb_name, ctx) in handles.kbs.iter() {
                match ctx
                    .storage
                    .retention_prune(now, history_secs, reading_secs)
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!(
                            kb = %kb_name.as_str(),
                            deleted = n,
                            "retention: pruned old history/reading rows"
                        );
                        ctx.bus.emit(
                            "maintenance.retention.pruned",
                            serde_json::json!({ "kb": kb_name.as_str(), "deleted": n }),
                        );
                    }
                    Err(e) => {
                        tracing::warn!(kb = %kb_name.as_str(), error = %e, "retention prune failed");
                    }
                }
            }
        }
    }))
}

/// L2 — daemon log-file retention. One background task that wakes once at
/// boot and then daily, deleting `<state>/log/kb.ndjson.*` files whose
/// mtime is older than `[server] log_retention_days` (validated ≥ 1;
/// default 14). ALWAYS spawned — unlike the R3 `[retention]` windows
/// (user data, opt-in) the logs are the daemon's own diagnostics and the
/// daily-rolled ndjson layer grows forever otherwise (the L1 disk-fill
/// risk). Deleting whole aged files is prune-not-edit, in the spirit of
/// invariant #8's retention exception. Watches the shutdown watch + is
/// joined in `teardown_tasks`, so an in-process config reload can't leak
/// it (and a reload picks up an edited window).
fn spawn_log_retention_prune(
    handles: Arc<crate::state::KbHandles>,
    retention_days: u32,
) -> tokio::task::JoinHandle<()> {
    use tokio::time::interval;
    let period = Duration::from_secs(kb_core::config::RetentionSection::PRUNE_INTERVAL_SECS);
    let max_age = std::time::Duration::from_secs(u64::from(retention_days.max(1)) * 86_400);
    let log_dir = handles.paths.log.clone();
    let mut shutdown = handles.shutdown.subscribe();
    tokio::spawn(async move {
        // `interval` fires its first tick immediately → one sweep at boot,
        // then once per period.
        let mut ticker = interval(period);
        loop {
            tokio::select! {
                _ = shutdown.wait_for(|&down| down) => break,
                _ = ticker.tick() => {}
            }
            let Some(cutoff) = std::time::SystemTime::now().checked_sub(max_age) else {
                continue; // clock predates the window — nothing can be older
            };
            // Sync fs walk + unlinks — keep them off the async workers.
            let dir = log_dir.clone();
            let pruned = tokio::task::spawn_blocking(move || {
                kb_core::tracing_init::prune_logs_older_than(&dir, cutoff)
            })
            .await;
            match pruned {
                Ok(Ok(0)) => {}
                Ok(Ok(n)) => {
                    tracing::info!(
                        deleted = n,
                        dir = %log_dir.display(),
                        retention_days,
                        "log retention: pruned aged ndjson log files"
                    );
                    handles.bus.emit(
                        "maintenance.logs.pruned",
                        serde_json::json!({ "deleted": n }),
                    );
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, dir = %log_dir.display(), "log retention sweep failed");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "log retention sweep task failed to run");
                }
            }
        }
    })
}

/// Future that resolves on SIGINT or SIGTERM (or, on non-unix, just
/// Ctrl-C). axum's `with_graceful_shutdown` awaits this; once it
/// completes, axum stops accepting new connections and waits for
/// in-flight ones to finish. Before returning it flips the `shutdown`
/// watch to `true`, which ends every `/api/events` SSE stream so the
/// drain isn't blocked by a subscription that never finishes on its own.
async fn shutdown_signal(
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    terminate_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    // CE — also complete when the watch is flipped externally (the
    // `/api/shutdown` route or `PUT /api/config`'s restart). Those callers
    // flip the watch directly + set their own outcome flags, so we must
    // NOT re-send and must NOT mark `terminate` (a config restart is not a
    // stop). Without this arm, `with_graceful_shutdown` would ignore the
    // watch and the drain would only fire via the deadline backstop.
    let mut external = shutdown_tx.subscribe();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
        _ = external.wait_for(|&d| d) => return,
    }
    // A genuine OS-signal stop: mark `terminate` so the boot loop exits
    // (rather than restarts), then flip the watch (ends SSE streams +
    // starts the drain clock).
    terminate_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    tracing::info!(
        grace_secs = GRACEFUL_DRAIN_SECS,
        "shutdown signal received; closing event streams + draining inflight requests"
    );
    // Ignore the error: a missing receiver just means nothing is listening
    // (no open SSE streams, no backstop yet) — the drain still proceeds.
    let _ = shutdown_tx.send(true);
}

/// Same as `serve_with_paths`, but binds to a kernel-assigned port
/// (`127.0.0.1:0`). Returns the bound address + a future that runs the
/// server. For tests; defaults to `spa_dist = None` (parent-origin
/// requests get the SPA-unavailable 404). Use
/// `serve_on_random_port_with_paths_and_spa` to wire a real dist.
pub async fn serve_on_random_port_with_paths(
    config: KbConfig,
    paths: KbPaths,
) -> Result<(std::net::SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    serve_on_random_port_with_paths_and_spa(config, paths, None).await
}

/// `serve_on_random_port_with_paths` variant taking an explicit SPA dist
/// path. Tests that exercise the SPA fallback (D2+) point this at a
/// fixture directory containing index.html and any test assets.
pub async fn serve_on_random_port_with_paths_and_spa(
    config: KbConfig,
    paths: KbPaths,
    spa_dist: Option<std::path::PathBuf>,
) -> Result<(std::net::SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    let paths = Arc::new(paths);
    paths.ensure_dirs().context("create state dirs")?;

    let started_at = chrono::Utc::now();
    let mut auth = crate::state::AuthConfig::load(&paths.token_file())
        .with_context(|| format!("read {}", paths.token_file().display()))?;
    // v0.34 Y1 — multi-user token registry + identity ladder config.
    auth.tokens = crate::state::AuthConfig::load_tokens_registry(&paths.tokens_file());
    auth.operator = config.identity.operator.clone();
    auth.identity_header = config.identity.header.to_ascii_lowercase();
    // v0.7.1 C1 — see `serve_with_paths`; same shared trusted-proxy Arc.
    let trusted_proxies = Arc::new(crate::state::parse_trusted_proxies(
        &config.server.trusted_proxies,
    ));
    auth.trusted_proxies = trusted_proxies.clone();
    let rate_limits = crate::state::RateLimits::from_section(config.server.rate_limit.as_ref());
    let origin = crate::state::OriginConfig {
        artifact_host_suffix: config.server.artifact_host_suffix.clone(),
        parent_origin: config.server.parent_origin.clone(),
        trusted_proxies: trusted_proxies.clone(),
    };
    let mut handles = KbHandles::new(paths.daemon_name.clone(), paths.clone(), started_at)
        .with_ui(config.ui.clone())
        .with_spa_dist(spa_dist)
        .with_auth(auth)
        .with_rate_limits(rate_limits)
        .with_origin(origin)
        .with_share(config.share.clone())
        .with_metrics(
            config.server.metrics,
            config.kb.keys().map(|k| k.as_str().to_string()),
        )
        // CE — populate config so /api/config GET/PUT tests work against the
        // random-port helper; config_path points at the rooted temp dir.
        .with_config(config.clone(), paths.config_file());

    install_corpus_mounts(&config);
    install_project_registry(&config);

    let mut embedders: std::collections::HashMap<String, Arc<Mutex<Embedder>>> =
        std::collections::HashMap::new();
    for (kb_name, kb_section) in &config.kb {
        let resolved_model = config.resolved_embedding_model(kb_section);
        // CE — test helper detaches the per-kb tasks (no restart loop); the
        // server task is aborted by the caller at test end.
        let ext_map = config.resolved_extension_map(kb_section);
        let (context, _kb_tasks) = bring_up_kb(
            kb_name,
            kb_section,
            resolved_model,
            &mut embedders,
            &config.indexer,
            &config.storage,
            &paths,
            &config.server.artifact_host_suffix,
            handles.bus.clone(),
            handles.pipeline.clone(),
            handles.shutdown.subscribe(),
            handles.review_lock_for(kb_name),
            ext_map,
        )
        .await
        .with_context(|| format!("bring up kb {kb_name}"))?;
        handles.insert(kb_name.clone(), context);
    }

    // v0.34 Y1 — same identity backfill as serve_with_paths (test harness).
    {
        let operator = handles.operator_user().to_string();
        let now_unix = chrono::Utc::now().timestamp();
        for (kb_name, ctx) in handles.kbs.iter() {
            match ctx
                .storage
                .identity_backfill(operator.clone(), now_unix)
                .await
            {
                Ok(n) if n > 0 => {
                    tracing::info!(
                        kb = %kb_name,
                        rows = n,
                        operator = %operator,
                        "identity backfill stamped operator on pre-multi-user rows"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(kb = %kb_name, error = %e, "identity_backfill failed");
                }
            }
        }
    }

    let handles = Arc::new(handles);
    // SW1 — run the 1Hz metrics ticker in the test fixture too, so
    // integration tests see the same `metrics.tick` stream (incl. the
    // `sse_subscribers` gauge) a production daemon emits. Detached like
    // the serve task below; it exits with the process.
    drop(spawn_metrics_ticker(handles.clone()));
    // L2 — run the log-retention sweep in the test fixture too (first tick
    // at boot), so integration tests can pin the boot prune. Detached like
    // the ticker above.
    drop(spawn_log_retention_prune(
        handles.clone(),
        config.server.log_retention_days,
    ));
    let app = build_router(handles);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .context("axum::serve")
    });
    Ok((addr, task))
}

/// `serve_on_random_port` reading paths from XDG. For binary entrypoints
/// or tests that want to exercise the production path resolution.
pub async fn serve_on_random_port(
    config: KbConfig,
) -> Result<(std::net::SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    let paths = build_xdg_paths(&config)?;
    serve_on_random_port_with_paths(config, paths).await
}

fn build_xdg_paths(config: &KbConfig) -> Result<KbPaths> {
    let daemon_name = config
        .daemon
        .name
        .clone()
        .unwrap_or_else(|| "default".to_string());
    KbPaths::new(&daemon_name).context("resolve XDG paths")
}

/// L4 — one-shot backfill: for every memory artifact in this
/// memory-scoped kb that has no V0011 `memory_links_seeded` row,
/// insert the `*` global sentinel + mark seeded. Pre-V0010 artifacts
/// then keep their "visible everywhere" behaviour after the migration
/// — matching today's "all memory corpora recall everywhere" mental
/// model. Idempotent: a fully-seeded kb returns after one `list_docs`
/// scan with zero writes.
async fn backfill_memory_links_seed(
    kb_name: &KbName,
    storage: &kb_core::storage::StorageHandle,
) -> Result<()> {
    use chrono::Utc;

    // u32::MAX is the "fetch everything" convention used elsewhere
    // (e.g. sessions list). Memory corpora are small (~hundreds of
    // rows in the worst case), so the full scan is cheap.
    let docs = storage.list_docs(u32::MAX).await.context("list_docs")?;
    let now = Utc::now().timestamp();
    let mut seeded = 0u64;
    for d in docs {
        // Only user-facing memory artifacts get seeded. `memory-session`
        // transcripts are excluded — they're session captures, not
        // user-level memories, and should only surface in the /sessions
        // view (not in every kb's Related-memories rail). Mirrors the
        // L3 indexer seed gate.
        let is_memory = d
            .kb_category
            .as_deref()
            .is_some_and(|c| c.starts_with("memory-") && c != "memory-session");
        if !is_memory {
            continue;
        }
        if storage
            .memory_links_seeded_has(d.id.clone())
            .await
            .with_context(|| format!("memory_links_seeded_has {}", d.id))?
        {
            continue;
        }
        // Pre-V0010 artifact: force-seed `*`. No explicit kb links
        // (we have no signal for them); the user/UI can add them
        // later. `memory_links_replace` with `linked_kbs = []` +
        // `global = true` writes exactly one row (the `*` sentinel).
        storage
            .memory_links_replace(d.id.clone(), Vec::new(), true, now)
            .await
            .with_context(|| format!("memory_links_replace {}", d.id))?;
        storage
            .memory_links_seeded_mark(d.id.clone(), now)
            .await
            .with_context(|| format!("memory_links_seeded_mark {}", d.id))?;
        seeded += 1;
    }
    if seeded > 0 {
        tracing::info!(kb = %kb_name, seeded, "memory link backfill: marked pre-V0010 memories as global");
    }
    Ok(())
}

// Eight inputs — one over clippy's threshold after CE added the shutdown
// receiver. They're all distinct boot parameters with no natural grouping
// that wouldn't just be a pass-through bag; an internal fn, single caller
// shape, so an allow is cleaner than a synthetic params struct.
/// S3 — build + install the process-global corpus mount table from config.
/// Every `[kb.*]` section contributes `(kb name, source path)`; the indexer's
/// session-capture hook resolves touched-file paths against it so a session
/// captured in `[kb.sessions]` links to the artifact it edited in another kb.
/// Idempotent; re-run on a config-reload restart.
fn install_corpus_mounts(config: &kb_core::config::KbConfig) {
    let mounts = config
        .kb
        .iter()
        .map(|(name, sec)| kb_core::sessions::CorpusMount {
            kb: name.as_str().to_string(),
            source_root: sec.path.clone(),
        })
        .collect();
    kb_core::sessions::set_corpus_mounts(mounts);
}

/// W3.A — build + install the process-global `[projects.*]` registry from
/// config (designs/projects.md P3), the `install_corpus_mounts` sibling.
/// `config.projects` is a `BTreeMap` so install order is the id's sort order
/// — deterministic, matching `[kb.*]`'s own convention, though not
/// necessarily the TOML file's literal declaration order (see
/// `kb_core::sessions::projects` module doc). Idempotent; re-run on a
/// config-reload restart.
fn install_project_registry(config: &kb_core::config::KbConfig) {
    let defs = config
        .projects
        .iter()
        .map(|(id, sec)| kb_core::sessions::ProjectDef {
            id: id.clone(),
            label: sec.label.clone().unwrap_or_else(|| id.clone()),
            roots: sec.roots.clone(),
            kb: sec.kb.clone(),
            code_url: sec.code_url.clone(),
            code_repo: sec.code_repo.clone(),
        })
        .collect();
    kb_core::sessions::set_project_registry(defs);
}

/// Get-or-spawn the shared embedder for `model` from `registry`. All kbs that
/// resolve to the same model name share ONE `kb-embedder` subprocess (clones
/// of one `Arc<Mutex<Embedder>>`) — the per-model, not per-kb, RAM fix
/// (~20×bge-large → 1×bge-large). The subprocess is kb-agnostic (just
/// `--model`/`--cache`) and accessed behind a mutex, so concurrent kbs'
/// embed calls serialise safely through it. Generic over the embedder type so
/// the dedup/sharing contract is unit-testable without a real subprocess.
fn shared_embedder<E>(
    registry: &mut std::collections::HashMap<String, Arc<Mutex<E>>>,
    model: &str,
    spawn: impl FnOnce() -> Result<E>,
) -> Result<Arc<Mutex<E>>> {
    if let Some(existing) = registry.get(model) {
        return Ok(existing.clone());
    }
    let arc = Arc::new(Mutex::new(spawn()?));
    registry.insert(model.to_string(), arc.clone());
    Ok(arc)
}

#[allow(clippy::too_many_arguments)]
async fn bring_up_kb(
    kb_name: &KbName,
    kb_section: &kb_core::config::KbSection,
    resolved_model: Option<&str>,
    embedders: &mut std::collections::HashMap<String, Arc<Mutex<Embedder>>>,
    indexer_section: &kb_core::config::IndexerSection,
    // Daemon-wide lance tuning (`[storage]` in kb.toml): cache caps + the
    // search-index rebuild throttle, resolved here and handed to
    // `Storage::open_with_options` via the actor.
    storage_section: &kb_core::config::StorageSection,
    paths: &Arc<KbPaths>,
    artifact_host_suffix: &str,
    bus: Arc<EventBus>,
    pipeline: Arc<kb_core::metrics::PipelineMetrics>,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    // R2 — the kb's shared per-kb comment lock (same instance the comment
    // routes use), handed to the indexer so its delete cascade can reap the
    // `.review` sidecar + attachments under it (invariants #6 / #18).
    review_lock: Arc<tokio::sync::Mutex<()>>,
    // X1 — the kb's resolved indexable-extension map (per-kb → `[indexer]` →
    // built-in default), pre-computed by the caller (which holds the full
    // `KbConfig`), exactly like `resolved_model`. Installed on the ingest sink
    // (walk/watcher/reconcile gates) AND handed to the indexer (parse dispatch).
    ext_map: kb_core::extmap::ExtensionMap,
) -> Result<(KbContext, Vec<tokio::task::JoinHandle<()>>)> {
    use kb_core::ids::SourceSlug;

    // CE — handles for the per-kb background tasks (indexer, reconciler,
    // history ring). Returned so the serve loop can join them on
    // shutdown/reload before dropping the kb's storage + embedder; each
    // task also watches `shutdown_rx` so it actually exits.
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    let lance_path = paths.kb_lance(kb_name);
    let sqlite_path = paths.kb_sqlite(kb_name);

    // Bake-off A1/B1: resolve the kb's configured embedding-model dim
    // (None when no `embedding_model` is configured — the daemon still
    // opens the dataset but won't enforce a width). `Storage::open`
    // uses this to detect a dim mismatch between disk and config, and
    // surfaces it as `Error::Config`. The error bubbles up from this
    // fn; the caller sees it via the `KbContext` `Result` and the
    // failing kb is skipped while siblings come up.
    //
    // D2 — `resolved_model` is the three-layer resolver output from
    // `KbConfig::resolved_embedding_model`. Production callers always
    // pass `Some(_)` for a registered model name; tests that want to
    // reproduce today's "no embedder" failure mode pass `None`.
    let model_info = resolved_model.and_then(kb_core::embed::model_info);
    let config_dim = model_info.map(|m| m.dim as i32);
    let lance_options = kb_core::storage::lance::LanceOptions {
        index_cache_mb: storage_section.resolved_lance_index_cache_mb(),
        metadata_cache_mb: storage_section.resolved_lance_metadata_cache_mb(),
        index_rebuild_min_secs: storage_section.resolved_index_rebuild_min_secs(),
    };
    let storage = StorageActor::spawn_with_metrics(
        lance_path,
        sqlite_path,
        config_dim,
        pipeline.clone(),
        lance_options,
    )
    .await?;

    // v0.33 X2 — bring-up seed for `doc_first_seen`. When the table is empty
    // and the corpus already has lance rows (upgrade path), seed every id
    // with coalesce(created_unix, mtime_unix, indexed_at_unix) so the
    // existing created-sort order is preserved. Fresh corpora stay empty
    // until the indexer success tail writes true first-index times.
    // Non-fatal: a seed failure only means first_indexed_unix is absent
    // until the next reindex pass fills rows one-by-one.
    match storage.first_seen_is_empty().await {
        Ok(true) => match storage.list_first_seen_seed_rows().await {
            Ok(rows) if !rows.is_empty() => {
                let seed: Vec<(String, i64)> = rows
                    .into_iter()
                    .filter_map(|(id, created, mtime, indexed)| {
                        kb_core::storage::sqlite::Db::first_seen_coalesce_ts(
                            created, mtime, indexed,
                        )
                        .map(|ts| (id, ts))
                    })
                    .collect();
                match storage.first_seen_seed(seed).await {
                    Ok(n) if n > 0 => {
                        tracing::info!(
                            kb = %kb_name,
                            seeded = n,
                            "doc_first_seen bring-up seed completed"
                        );
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(
                            kb = %kb_name,
                            error = %e,
                            "doc_first_seen bring-up seed failed (non-fatal)"
                        );
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    kb = %kb_name,
                    error = %e,
                    "list_first_seen_seed_rows failed (non-fatal)"
                );
            }
        },
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(
                kb = %kb_name,
                error = %e,
                "first_seen_is_empty failed (non-fatal)"
            );
        }
    }

    // v0.7.1 H5 — `bus` is the single daemon-wide EventBus, shared by
    // every kb (not a fresh per-kb bus). The watcher + indexer below
    // tag every envelope with `kb`, and the indexer + history task
    // filter on it, so one bus carries all kbs with a single
    // monotonic id space.

    let source_path = kb_section
        .path
        .canonicalize()
        .unwrap_or_else(|_| kb_section.path.clone());
    let source_slug = SourceSlug::from_path(&source_path);
    // Track V — resolve the per-kb versions mode + locate the git repo that
    // owns this corpus (possibly an ancestor of source_path), both once here.
    let versions_mode = kb_section
        .versions
        .as_deref()
        .and_then(|s| {
            let m = kb_core::vcs::VersionsMode::parse(s);
            if m.is_none() {
                tracing::warn!(
                    kb = %kb_name,
                    value = s,
                    "ignoring unknown [kb.*] versions; expected auto|git|index|both|off"
                );
            }
            m
        })
        .unwrap_or_default();
    let git_root = kb_core::vcs::find_git_root(&source_path);
    let now = chrono::Utc::now().timestamp();
    storage
        .upsert_source(source_slug.clone(), source_path.clone(), now)
        .await?;

    // FU1 — shared DedupCache for this kb (empty until the indexer task
    // pre-populates from lance). Relocate routes + startup replay rekey
    // through the same Arc so a move does not force a re-embed.
    let dedup = kb_core::indexer::empty_dedup_cache();

    // F3b/F3c — startup replay of incomplete `moves` rows (non-fatal).
    // MUST run BEFORE the watcher + initial reconcile start below so a
    // crash-before-rename intent is abandoned (completed_at set) before any
    // watcher event can be suppressed by moves_suppresses_delete.
    // FU1: pass the shared dedup for uniformity with runtime relocate; the
    // cache is still empty here (indexer has not started), so rekey is a
    // harmless no-op. Lance already holds the post-move ids after a
    // completed rekey; the indexer warms from that snapshot next.
    {
        let replay_ctx = kb_core::relocate::RelocateCtx {
            storage: storage.clone(),
            source_root: source_path.clone(),
            review_dir: paths.kb_review_dir(kb_name),
            review_lock: Some(review_lock.clone()),
            pending: kb_core::relocate::shared_pending(),
            dedup: Some(dedup.clone()),
            kb: kb_name.clone(),
        };
        match kb_core::relocate::replay_incomplete_moves(&replay_ctx).await {
            Ok(n) if n > 0 => {
                tracing::info!(
                    kb = %kb_name,
                    fixed = n,
                    "moves startup replay completed"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(
                    kb = %kb_name,
                    error = %e,
                    "moves startup replay failed; continuing boot"
                );
            }
        }
    }

    // Construct the per-kb embedder if a model is configured. The first
    // call downloads ~130 MB of model weights to the XDG cache (one-time;
    // subsequent daemon starts hit the cache in ~3 s per spike-fastembed).
    // Tests that don't want the download pass resolved_model = None.
    //
    // D2 — the resolved name + dim are logged here so operators can see at
    // a glance which model each kb is opening (helpful when the [defaults]
    // section is in play and the per-kb section omits the field).
    let embedder: Option<Arc<Mutex<Embedder>>> = if let Some(info) = model_info {
        let cache_dir = models_cache_dir(&paths.cache);
        let nice = indexer_section.resolved_indexer_nice();
        let reused = embedders.contains_key(info.name);
        tracing::info!(
            kb = %kb_name,
            model = %info.name,
            dim = info.dim,
            nice,
            reused,
            "kb opened: {} kb-embedder subprocess",
            if reused { "sharing" } else { "spawning" }
        );
        Some(shared_embedder(embedders, info.name, || {
            Embedder::spawn_ipc(info.name, cache_dir, nice)
                .with_context(|| format!("spawn embedder {}", info.name))
        })?)
    } else {
        tracing::info!(
            kb = %kb_name,
            "kb opened: no embedding model configured (semantic/hybrid disabled)"
        );
        None
    };

    // SQ4 — optional cross-encoder reranker. Opt-in per kb; spawns a
    // `kb-embedder --reranker` subprocess (niced like the embedder) so the
    // daemon links no ONNX. The model load is slow, so the spawn+handshake
    // runs in spawn_blocking. Non-fatal: a load failure (download error /
    // missing binary) logs and leaves the reranker disabled, so search
    // keeps working.
    let reranker: Option<Arc<Mutex<kb_core::embed_ipc::RerankerClient>>> = if let Some(rname) =
        kb_section.reranker_model.as_deref()
    {
        let cache_dir = models_cache_dir(&paths.cache);
        let nice = indexer_section.resolved_indexer_nice();
        let rname_owned = rname.to_string();
        let kb_for_log = kb_name.clone();
        match tokio::task::spawn_blocking(move || {
            kb_core::embed_ipc::RerankerClient::spawn_ipc(&rname_owned, cache_dir, nice)
        })
        .await
        {
            Ok(Ok(r)) => {
                tracing::info!(kb = %kb_for_log, reranker = %rname, "kb opened: reranker loaded");
                Some(Arc::new(Mutex::new(r)))
            }
            Ok(Err(e)) => {
                tracing::warn!(kb = %kb_for_log, reranker = %rname, error = %e,
                        "reranker failed to load; search continues without reranking");
                None
            }
            Err(e) => {
                tracing::warn!(kb = %kb_for_log, error = %e,
                        "reranker load task panicked; search continues without reranking");
                None
            }
        }
    } else {
        None
    };

    // G7 — per-kb back-pressured ingest channel. Every producer (watcher,
    // reconciler, reindex route) pushes `WatchWork` through the `ingest` sink;
    // the indexer drains `ingest_rx`. The indexer reads ONLY this channel
    // (never the shared bus), so unrelated bus traffic can't make it lag, and a
    // bulk burst back-pressures the producer instead of dropping. The sink also
    // mirrors each event to the bus as a `watch.*` frame for observability
    // (TUI heatmap, SPA event log).
    let (ingest_tx, ingest_rx) =
        tokio::sync::mpsc::channel(kb_core::indexer::INGEST_QUEUE_CAPACITY);
    // X2 — the kb's shared ingest gate: load the durable sqlite truth
    // (excluded_files + sources.paused) into the runtime snapshot every
    // enforcement seam reads. Both loads are best-effort-with-warning: an
    // unreadable table degrades to "nothing excluded / unpaused" (the
    // pre-X2 behaviour) rather than failing bring-up.
    let gate = kb_core::exclusions::IngestGate::default();
    match storage.list_exclusions().await {
        Ok(rows) => {
            if !rows.is_empty() {
                tracing::info!(kb = %kb_name, excluded = rows.len(), "loaded per-file exclusions");
            }
            gate.set_excluded(rows.into_iter().map(|r| r.path));
        }
        Err(e) => {
            tracing::warn!(kb = %kb_name, error = %e, "list_exclusions failed; exclusion gate starts empty");
        }
    }
    match storage.list_sources().await {
        Ok(rows) => {
            let paused = rows
                .iter()
                .any(|s| s.raw_slug == source_slug.as_str() && s.paused);
            if paused {
                tracing::info!(kb = %kb_name, "source is paused; ingest gated until resume (D6)");
            }
            gate.set_paused(paused);
        }
        Err(e) => {
            tracing::warn!(kb = %kb_name, error = %e, "list_sources failed; paused gate starts open");
        }
    }
    // X1 — install the resolved extension map on the sink so every producer
    // that holds it (watcher initial-walk + live gates, reconciler walk +
    // delete pass, reindex route) shares one ingest gate. X2 — same for the
    // exclusion/paused gate (cloning the sink shares the SAME gate; the
    // pause/resume route + exclusion ops flip it live via `ctx.ingest.gate()`).
    let ingest = kb_core::indexer::IngestSink::new(ingest_tx, bus.clone(), kb_name.clone())
        .with_extensions(ext_map.clone())
        .with_gate(gate.clone());

    let kb_for_indexer = kb_name.clone();
    let slug_for_indexer = source_slug.clone();
    let storage_for_indexer = storage.clone();
    let bus_for_indexer = bus.clone();
    let quarantine_dir = paths.quarantine_kb_dir(kb_name);
    let embedder_for_indexer = embedder.clone();
    // SQ5 — whether the indexer also produces passage chunks for this kb.
    let chunked_for_indexer = kb_section.chunked_embeddings;
    let pipeline_for_indexer = pipeline.clone();
    // X1 — the same resolved map the sink carries; the indexer uses it at the
    // parse-dispatch seam (Html vs Markdown).
    let ext_map_for_indexer = ext_map.clone();
    // X2 — the same shared gate the sink carries; the indexer consults it per
    // item at the top of `prepare_doc`.
    let gate_for_indexer = gate.clone();
    // FU1 — same shared DedupCache the routes rekey on relocate.
    let dedup_for_indexer = dedup.clone();
    // v0.3 G2 — review dir feeds the indexer's anchor-stale check.
    // Always provided in the daemon path; tests typically pass None.
    let review_dir_for_indexer = Some(paths.kb_review_dir(kb_name));
    let review_lock_for_indexer = review_lock;
    let artifact_host_suffix_for_indexer = artifact_host_suffix.to_string();
    // Resolved here for the periodic reconcile-loop setup below (`Copy`, so the
    // `async move` capture leaves the value usable there). PF-I1 —
    // `resolved_reconcile_secs_for` layers `kb_section.reconcile_secs`
    // between the `KB_RECONCILE_SECS` env escape hatch (still trumps
    // everything) and the daemon-wide `[indexer] reconcile_secs`. The
    // periodic auto-compact ticker below (`compact_check_interval_secs`) is
    // ALSO spawned per kb inside this same function call, so it inherits
    // this per-kb value for free — no separate resolution needed there.
    let reconcile_secs = indexer_section.resolved_reconcile_secs_for(kb_section);
    let mut idx_shutdown = shutdown_rx.clone();
    tasks.push(tokio::spawn(async move {
        // CE — race the indexer against shutdown so a reload drops the future
        // (releasing its bus/storage/embedder clones + the ingest receiver)
        // rather than leaking the task + its kb-embedder subprocess. The
        // indexer does fire-and-forget writes through the storage actor, so
        // dropping it mid-flight is cancel-safe (a dropped oneshot just skips
        // the ack).
        tokio::select! {
            _ = kb_core::indexer::run_with_ingest(
                kb_for_indexer,
                slug_for_indexer,
                storage_for_indexer,
                bus_for_indexer,
                quarantine_dir,
                ingest_rx,
                embedder_for_indexer,
                review_dir_for_indexer,
                Some(review_lock_for_indexer),
                artifact_host_suffix_for_indexer,
                versions_mode,
                pipeline_for_indexer,
                chunked_for_indexer,
                ext_map_for_indexer,
                gate_for_indexer,
                dedup_for_indexer,
            ) => {}
            _ = idx_shutdown.wait_for(|&d| d) => {}
        }
    }));

    // Watcher (drop = stop). Hold it inside the KbContext.
    // v0.7.x — debounce window respects `[indexer] debounce_ms` in
    // kb.toml + the `KB_DEBOUNCE_MS` env override (env wins). Pre-v0.7.x
    // the doc claimed env-overrideable but the code hardcoded the
    // default; this resolves that drift.
    let debounce_ms = indexer_section.resolved_debounce_ms();
    tracing::info!(
        kb = %kb_name,
        debounce_ms,
        "starting watcher",
    );
    // SC1 — seed the initial walk with the stored (path → mtime) snapshot
    // (the reconcile pass's G5 projection, fetched ONCE here) so a restart
    // over an already-indexed corpus walks stat-only and emits ~nothing,
    // instead of one create per file — each of which cost a read+hash and,
    // pre-SC1, an unconditional full-table-scan `touch_mtime` UPDATE
    // commit (the restart-with-backlog storm). Best-effort: on error the
    // walk falls back to full emission and the indexer's pre-gate dedups.
    let walk_known_mtimes: std::collections::HashMap<std::path::PathBuf, i64> =
        match storage.list_reconcile_rows().await {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|(_id, path, mtime)| mtime.map(|m| (std::path::PathBuf::from(path), m)))
                .collect(),
            Err(e) => {
                tracing::warn!(
                    kb = %kb_name, error = %e,
                    "initial-walk mtime seed unavailable; walking the full corpus",
                );
                Default::default()
            }
        };
    let watcher = Watcher::start(
        WatcherConfig::new(kb_name.clone(), vec![source_path.clone()])
            .with_debounce(Duration::from_millis(debounce_ms))
            .with_skip_patterns(kb_section.skip_patterns.clone())
            .with_watch_mode(indexer_section.resolved_watch_mode())
            .with_poll_interval(Duration::from_millis(
                indexer_section.resolved_poll_interval_ms(),
            ))
            .with_known_mtimes(walk_known_mtimes),
        ingest.clone(),
    )?;

    // v0.7.x — periodic reconciliation pass. The notify-backed watcher
    // is fast in the common case but has known gaps: inotify queue
    // overflow on large bursts, NFS/FUSE mounts that don't deliver
    // events, the small race between the watcher's initial walk and its
    // arm-debouncer step, and dropped broadcast lag. The reconciler
    // walks the source folder on a fixed interval and emits
    // `watch.modify` envelopes for every `*.html`/`*.htm`; the
    // indexer's content-hash dedup gate makes byte-identical files
    // cheap, so the pass is near-free when nothing changed and acts as
    // a safety net for any class of missed event. Setting
    // `[indexer] reconcile_secs = 0` (or `KB_RECONCILE_SECS=0`, or —
    // PF-I1 — this kb's own `[kb.<name>] reconcile_secs = 0`)
    // disables. The first tick fires after `interval`, not zero, so it
    // doesn't pile on top of the watcher's startup initial walk.
    // (`reconcile_secs` was resolved above, before the indexer spawn.)
    // R1 — shared snapshot of the last completed reconcile pass. Lives
    // on the KbContext so /api/stats can read it; written by the
    // reconcile loop below on every tick. Wrapped in a sync `Mutex`
    // (not async) because writers and readers both hold it for
    // microseconds at most.
    let last_reconcile: Arc<std::sync::Mutex<Option<kb_core::indexer::ReconcileSummary>>> =
        Arc::new(std::sync::Mutex::new(None));
    if reconcile_secs > 0 {
        let bus_for_reconcile = bus.clone();
        let ingest_for_reconcile = ingest.clone();
        let kb_for_reconcile = kb_name.clone();
        let path_for_reconcile = source_path.clone();
        let storage_for_reconcile = storage.clone();
        let skips_for_reconcile = kb_section.skip_patterns.clone();
        let last_reconcile_for_task = last_reconcile.clone();
        let mut rec_shutdown = shutdown_rx.clone();
        let rec_handle = tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(reconcile_secs);
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            // Skip ticks we couldn't run on time (e.g. an oversized
            // walk overran the interval) — we just want "walk again
            // soon", not a queued backlog.
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                // CE — exit on shutdown; this interval loop otherwise never
                // ends and would leak (doubling reconcile passes) on reload.
                tokio::select! {
                    _ = rec_shutdown.wait_for(|&d| d) => break,
                    _ = ticker.tick() => {}
                }
                let summary = kb_core::indexer::reconcile(
                    ingest_for_reconcile.clone(),
                    storage_for_reconcile.clone(),
                    path_for_reconcile.clone(),
                    skips_for_reconcile.clone(),
                )
                .await;
                tracing::info!(
                    kb = %kb_for_reconcile,
                    files = summary.files_walked,
                    deletes = summary.deletes_emitted,
                    elapsed_ms = summary.duration_ms,
                    "reconciliation pass complete",
                );
                // R1 — stash for /api/stats. The lock is uncontended
                // outside this writer + the route handler readers.
                if let Ok(mut guard) = last_reconcile_for_task.lock() {
                    *guard = Some(summary);
                }
                // W1+: emit an SSE event so observability isn't
                // tracing-only. SPA / TUI subscribers can show
                // reconcile activity in the same activity log as the
                // live watcher events. Deep-review cross-cutting note.
                bus_for_reconcile.emit(
                    "reconcile.complete",
                    serde_json::json!({
                        "kb": kb_for_reconcile.as_str(),
                        "files": summary.files_walked,
                        "deletes": summary.deletes_emitted,
                        "duration_ms": summary.duration_ms,
                    }),
                );
                if summary.duration_ms > interval.as_millis() as u64 {
                    tracing::warn!(
                        kb = %kb_for_reconcile,
                        elapsed_ms = summary.duration_ms,
                        interval_ms = interval.as_millis() as u64,
                        "reconciliation pass slower than interval; consider raising reconcile_secs",
                    );
                }
            }
        });
        tasks.push(rec_handle);
        tracing::info!(
            kb = %kb_name,
            reconcile_secs,
            "reconciliation pass scheduled",
        );
    } else {
        tracing::info!(
            kb = %kb_name,
            "reconciliation pass disabled (reconcile_secs=0)",
        );
    }

    // SC1 (scale-lab flag a) — periodic auto-compact on its OWN ticker,
    // decoupled from the reconcile loop it used to ride as a tail call.
    // That coupling failed both ways: `reconcile_secs = 0` silently
    // disabled periodic compaction entirely, and during a bulk drain the
    // reconcile pass blocks on the full ingest channel for the whole
    // backlog (its emissions back-pressure), so the compact could never
    // run in exactly the fragment-heavy window it exists for (measured:
    // zero compaction passes across a 20k drain, 625+ fragments at
    // completion). The check itself is cheap manifest reads; the optimize
    // only runs when `wants_compaction` trips, and `compact_off_loop`
    // parks concurrent lance mutations, so firing mid-drain is safe and
    // keeps merge_insert costs from creeping with fragment count.
    //
    // This ticker is spawned once PER KB (this whole function runs once per
    // `[kb.*]` entry), so it's already per-kb in shape; PF-I1 makes it
    // follow the per-kb `reconcile_secs` too, for free, since it derives
    // from the SAME local `reconcile_secs` that PF-I1 resolved above via
    // `resolved_reconcile_secs_for` (no separate resolution call needed
    // here — `compact_check_interval_secs` itself stays a pure fn of
    // whatever interval it's handed).
    {
        let compact_secs = compact_check_interval_secs(reconcile_secs);
        let storage_for_compact = storage.clone();
        let bus_for_compact = bus.clone();
        let kb_for_compact = kb_name.clone();
        let mut cmp_shutdown = shutdown_rx.clone();
        tasks.push(tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(compact_secs);
            let mut ticker =
                tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = cmp_shutdown.wait_for(|&d| d) => break,
                    _ = ticker.tick() => {}
                }
                maybe_compact_kb(
                    kb_for_compact.as_str(),
                    &storage_for_compact,
                    &bus_for_compact,
                    true,
                )
                .await;
            }
        }));
    }

    // v0.3 G3 — pre-compile any outbound scrubbing rules from kb.toml
    // so the artifact serve handler doesn't pay the regex compile cost
    // per-request. Invalid patterns warn + are dropped (see OutboundCache::from_section).
    let outbound = kb_section
        .outbound
        .as_ref()
        .and_then(crate::state::OutboundCache::from_section);

    // v0.5 P2 — resolve [kb.foo.atlas] overrides at boot. `None` k
    // = √n default; layout = Umap unless explicitly "pca".
    let atlas = crate::state::AtlasOverrides::from_section(kb_section.atlas.as_ref());

    // v0.6 R1 — per-kb run + query history rings. One background task
    // subscribes to the firehose and routes the three relevant event
    // kinds (`index.start`, `index.complete`, `query`) into the rings.
    // The task dies naturally when the bus is dropped on daemon shutdown.
    let runs = Arc::new(kb_core::history::RunsRing::default());
    let queries = Arc::new(kb_core::history::QueriesRing::default());
    let runs_for_task = runs.clone();
    let queries_for_task = queries.clone();
    let kb_for_history = kb_name.clone();
    let mut history_rx = bus.subscribe();
    let mut hist_shutdown = shutdown_rx.clone();
    tasks.push(tokio::spawn(async move {
        loop {
            tokio::select! {
                // CE — also exit on shutdown (not only on bus-close, which
                // can't fire while other tasks still hold bus senders).
                _ = hist_shutdown.wait_for(|&d| d) => break,
                r = history_rx.recv() => match r {
                    Ok(env) => {
                        // v0.7.1 H5 — the bus is daemon-wide now, so filter
                        // to this kb's events before feeding its rings
                        // (mirrors the indexer's own kb filter).
                        if env.payload["kb"].as_str() != Some(kb_for_history.as_str()) {
                            continue;
                        }
                        kb_core::history::apply_envelope(&runs_for_task, &queries_for_task, &env);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }));

    // L4 — one-shot backfill seeder. For every memory artifact in
    // this memory-scoped kb that has no V0011 `memory_links_seeded`
    // row yet, write the `*` global sentinel + mark seeded. Keeps
    // back-compat: pre-V0010 memories stay "visible everywhere",
    // matching today's "all memory corpora recall everywhere"
    // mental model. Idempotent — re-running over a fully-seeded kb
    // is a no-op aside from one `list_docs` scan.
    if kb_section.memory_scope.is_some() {
        if let Err(e) = backfill_memory_links_seed(kb_name, &storage).await {
            tracing::warn!(
                kb = %kb_name,
                error = %e,
                "memory link backfill failed; pre-V0010 memories may not be globally visible",
            );
        }
    }

    Ok((
        KbContext {
            kb_name: kb_name.clone(),
            source_path,
            source_slug,
            storage,
            bus,
            ingest,
            _watcher: Arc::new(watcher),
            embedder,
            reranker,
            chunked: kb_section.chunked_embeddings,
            graph_boost: kb_section.graph_boost,
            typo_tolerance: kb_section.search.typo_tolerance,
            outbound,
            atlas,
            resurface: kb_core::resurface::ResurfaceWeights::from_section(
                kb_section.resurface.as_ref(),
            ),
            runs,
            queries,
            skip_patterns: kb_section.skip_patterns.clone(),
            last_reconcile,
            reconcile_secs,
            memory_scope: kb_section.memory_scope.clone(),
            default_search_category: kb_section.default_search_category.clone(),
            // CT-F5 — an absent `[kb.*.slo]` resolves to the all-`None`
            // default, which still yields a full report (measured, unjudged).
            slo_targets: kb_section.slo.unwrap_or_default().targets(),
            code_url: kb_section.code_url.clone(),
            memory_decay_policy: kb_section.decay_policy.as_deref().and_then(|s| {
                let parsed = kb_core::memory::DecayPolicy::parse(s);
                if parsed.is_none() {
                    tracing::warn!(
                        kb = %kb_name,
                        value = s,
                        "ignoring unknown [kb.*] decay_policy; expected strict|balanced|loose"
                    );
                }
                parsed
            }),
            git_root,
            versions_mode,
            reading_progress: kb_section.reading_progress_enabled(),
            gallery_cache: Arc::new(std::sync::Mutex::new(None)),
            links_cache: Arc::new(std::sync::Mutex::new(None)),
            edges_cache: Arc::new(std::sync::Mutex::new(None)),
            facets_cache: Arc::new(std::sync::Mutex::new(None)),
            atlas_points_cache: Arc::new(std::sync::Mutex::new(None)),
            atlas_recompute: Arc::new(std::sync::Mutex::new(None)),
            // SC5 — same resolved map already installed on `ingest` +
            // `ext_map_for_indexer` above; last use, so moved (not cloned).
            ext_map,
            // FU1 — shared with indexer task (clone already moved into spawn).
            dedup,
        },
        tasks,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// kb-sibling/1 — the per-kb boot guard, as this daemon experiences it.
    /// `bring_up_kb` opens every kb's `index.db` through
    /// `StorageActor::spawn` → `kb_core::storage::sqlite::Db::open`, which
    /// refuses a volume a NEWER binary already forward-migrated; the error
    /// propagates all the way out of `serve_with_paths`, so the daemon does
    /// not come up at all. The too-new history row is FABRICATED (real
    /// migrations are immutable) at `schema_epoch() + 1000`.
    // invariant:2 kb-sibling/1 schema-epoch boot refuse
    #[test]
    fn per_kb_index_db_refuses_to_open_when_the_volume_epoch_is_ahead() {
        use kb_core::storage::sqlite::{schema_epoch, Db};

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.db");
        let ahead = schema_epoch() + 1_000;
        Db::open(&path).expect("first open migrates normally");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO refinery_schema_history (version, name, applied_on, checksum) \
                 VALUES (?1, 'from_a_newer_binary', '', '0')",
                [ahead],
            )
            .unwrap();
        }

        let msg = match Db::open(&path) {
            Ok(_) => panic!("a forward-migrated volume must refuse to open"),
            Err(e) => e.to_string(),
        };
        assert!(msg.contains("refusing to boot"), "{msg}");
        assert!(msg.contains(&format!("V{ahead}")), "{msg}");
        assert!(msg.contains(&format!("V{}", schema_epoch())), "{msg}");
        assert!(msg.contains("index.db"), "{msg}");
    }

    /// The passing case: a volume at the SAME epoch as this binary opens
    /// normally (the steady-state boot every daemon start performs).
    #[test]
    fn per_kb_index_db_opens_when_the_volume_epoch_equals_the_binary_epoch() {
        use kb_core::storage::sqlite::{schema_epoch, Db};

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("index.db");
        Db::open(&path).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let on_disk: u32 = conn
            .query_row(
                "SELECT MAX(version) FROM refinery_schema_history",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(on_disk, schema_epoch());
        drop(conn);
        Db::open(&path).expect("re-opening at an equal epoch must boot");
    }

    /// A freshly compacted dataset (few small fragments, few versions)
    /// must NOT compact in either mode.
    #[test]
    fn wants_compaction_healthy_is_false() {
        assert!(!wants_compaction(91, 1, 0, 5, false));
        assert!(!wants_compaction(91, 1, 0, 5, true));
        assert!(!wants_compaction(10_000, 200, 4, 50, true));
    }

    /// The small-fragment trigger fires in BOTH modes — it's the signal
    /// that tracks real latency rot and is self-resolving (compaction
    /// SC1 — the periodic auto-compact ticker is decoupled from the
    /// reconcile loop: `reconcile_secs = 0` no longer disables it (it
    /// falls back to the default cadence), and a configured reconcile
    /// interval keeps the pre-SC1 rhythm.
    #[test]
    fn compact_check_interval_decouples_from_disabled_reconcile() {
        assert_eq!(compact_check_interval_secs(0), COMPACT_CHECK_SECS_DEFAULT);
        assert_eq!(compact_check_interval_secs(60), 60);
        assert_eq!(compact_check_interval_secs(3600), 3600);
    }

    /// merges them away, so it won't re-fire).
    #[test]
    fn wants_compaction_small_fragments_fires_both_modes() {
        assert!(wants_compaction(91, 60, COMPACT_SMALL_FRAGMENTS, 5, true));
        assert!(wants_compaction(91, 60, COMPACT_SMALL_FRAGMENTS, 5, false));
        // One under the bar → no compaction.
        assert!(!wants_compaction(
            91,
            60,
            COMPACT_SMALL_FRAGMENTS - 1,
            5,
            true
        ));
    }

    /// The version cap fires at STARTUP (old versions are prunable on a
    /// fresh boot) but is deliberately IGNORED in the periodic path —
    /// recent versions can't be pruned inside lance's retention window, so
    /// honouring it there compacts every reconcile tick forever (the storm
    /// the end-to-end test caught).
    #[test]
    fn wants_compaction_version_cap_is_startup_only() {
        assert!(wants_compaction(
            91,
            1,
            2,
            COMPACT_MAX_VERSIONS + 100,
            false
        ));
        assert!(!wants_compaction(
            91,
            1,
            2,
            COMPACT_MAX_VERSIONS + 100,
            true
        ));
    }

    /// The fragments-per-row ratio is also startup-only (it only trips on
    /// huge corpora), and `rows == 0` guards the ratio term.
    #[test]
    fn wants_compaction_ratio_is_startup_only_and_guards_empty() {
        assert!(wants_compaction(
            10,
            10 * (COMPACT_FRAGMENTS_PER_ROW + 1),
            2,
            5,
            false
        ));
        assert!(!wants_compaction(
            10,
            10 * (COMPACT_FRAGMENTS_PER_ROW + 1),
            2,
            5,
            true
        ));
        // rows == 0 → ratio suppressed.
        assert!(!wants_compaction(0, 1000, 2, 5, false));
        assert!(!wants_compaction(0, 0, 0, 0, true));
    }

    /// Shared-embedder dedup contract: kbs on the same model resolve to ONE
    /// shared handle (spawn runs once); distinct models get distinct handles.
    /// Generic in `shared_embedder`, so `u32` stands in for a real Embedder.
    #[test]
    fn shared_embedder_shares_one_per_model() {
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicU32, Ordering};
        let mut reg: HashMap<String, Arc<Mutex<u32>>> = HashMap::new();
        let spawns = AtomicU32::new(0);
        let get = |reg: &mut HashMap<String, Arc<Mutex<u32>>>, model: &str, val: u32| {
            shared_embedder(reg, model, || {
                spawns.fetch_add(1, Ordering::SeqCst);
                Ok(val)
            })
            .unwrap()
        };
        let a1 = get(&mut reg, "bge-large", 1);
        let a2 = get(&mut reg, "bge-large", 2); // same model → reuse (val ignored)
        let b1 = get(&mut reg, "bge-small", 9);
        // bge-large spawned once + bge-small once = 2 subprocesses, not 3.
        assert_eq!(spawns.load(Ordering::SeqCst), 2);
        // Same model → the SAME Arc (one shared subprocess).
        assert!(Arc::ptr_eq(&a1, &a2));
        assert!(!Arc::ptr_eq(&a1, &b1));
        // The reused handle kept the first spawn's value (spawn didn't re-run).
        assert_eq!(*a2.lock().unwrap(), 1);
        assert_eq!(reg.len(), 2);
    }

    /// A wedged drain (server future that never completes, like an SSE
    /// connection that ignored the close) must not block exit past the
    /// grace period once shutdown fires.
    #[tokio::test]
    async fn drain_deadline_forces_exit_when_server_hangs() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let never = std::future::pending::<Result<()>>();
        let grace = Duration::from_millis(80);
        let task = tokio::spawn(run_with_drain_deadline(never, rx, grace));

        // Before the signal, the deadline clock hasn't started — the helper
        // stays pending even past what would be the grace window.
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            !task.is_finished(),
            "must keep serving until shutdown fires"
        );

        tx.send(true).unwrap();
        // Now the grace clock runs; the helper resolves shortly after.
        let res = tokio::time::timeout(Duration::from_secs(2), task).await;
        assert!(res.is_ok(), "must force-exit within the grace window");
        assert!(
            res.unwrap().expect("join").is_ok(),
            "deadline path returns Ok"
        );
    }

    /// When the server future completes on its own (drain finished), the
    /// helper returns the server's result rather than waiting out the grace.
    /// The 60s grace would dominate if the deadline arm were taken — the
    /// test passing instantly proves the server arm wins.
    #[tokio::test]
    async fn drain_deadline_returns_server_result_when_it_completes() {
        // Already shutting down, but the server drains immediately.
        let (_tx, rx) = tokio::sync::watch::channel(true);
        let server = async { Ok::<(), anyhow::Error>(()) };
        let res = run_with_drain_deadline(server, rx, Duration::from_secs(60)).await;
        assert!(res.is_ok(), "server result is propagated");
    }

    /// A server error is propagated, not swallowed by the deadline arm.
    #[tokio::test]
    async fn drain_deadline_propagates_server_error() {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let server = async { Err::<(), anyhow::Error>(anyhow::anyhow!("bind failed")) };
        let res = run_with_drain_deadline(server, rx, Duration::from_secs(60)).await;
        assert!(res.is_err(), "server error surfaces");
    }

    // ---- X1 — event→webhook bridge ----

    /// No config / empty url / empty types → no task spawned. An
    /// unconfigured daemon must not start a webhook subscriber.
    #[test]
    fn webhook_bridge_disabled_when_unconfigured() {
        let bus = Arc::new(EventBus::default());
        let (_tx, rx) = tokio::sync::watch::channel(false);
        assert!(spawn_webhook_bridge(bus.clone(), rx.clone(), None).is_none());
        assert!(spawn_webhook_bridge(
            bus.clone(),
            rx.clone(),
            Some(kb_core::config::WebhooksSection {
                url: "  ".into(),
                types: vec!["artifact.indexed".into()],
                timeout_ms: None,
                allow_private: false,
            })
        )
        .is_none());
        assert!(spawn_webhook_bridge(
            bus.clone(),
            rx.clone(),
            Some(kb_core::config::WebhooksSection {
                url: "http://127.0.0.1:1/hook".into(),
                types: vec![],
                timeout_ms: None,
                allow_private: false,
            })
        )
        .is_none());
        // SSRF default: private targets refuse at spawn (no task).
        assert!(spawn_webhook_bridge(
            bus,
            rx,
            Some(kb_core::config::WebhooksSection {
                url: "http://169.254.169.254/latest/meta-data/".into(),
                types: vec!["artifact.indexed".into()],
                timeout_ms: None,
                allow_private: false,
            })
        )
        .is_none());
    }

    // ---- R3 — opt-in retention prune ----

    /// The spawn decision: an unset `[retention]` yields no windows (→ no
    /// task); a set window resolves to seconds. Mirrors the "no idle task
    /// when the feature is off" invariant without building a daemon.
    #[test]
    fn retention_windows_off_when_unset_on_when_set() {
        use kb_core::config::RetentionSection;
        const DAY: i64 = 86_400;

        let off = RetentionSection::default();
        assert!(
            retention_windows(&off).is_none(),
            "no window set → no prune task"
        );

        let history_only = RetentionSection {
            history_days: Some(30),
            reading_sections_days: None,
        };
        assert_eq!(
            retention_windows(&history_only),
            Some((Some(30 * DAY), None)),
            "a history window arms the task"
        );

        let both = RetentionSection {
            history_days: Some(30),
            reading_sections_days: Some(7),
        };
        assert_eq!(
            retention_windows(&both),
            Some((Some(30 * DAY), Some(7 * DAY)))
        );
    }

    /// End-to-end: the bridge POSTs only envelopes whose `type` is in the
    /// allowlist, and the task joins when the shutdown watch flips.
    #[tokio::test]
    async fn webhook_bridge_forwards_only_selected_types_and_joins_on_shutdown() {
        use axum::{routing::post, Json, Router};

        // A tiny sink server that records the `type` of each event it gets.
        let received = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = received.clone();
        let app = Router::new().route(
            "/hook",
            post(move |body: Json<serde_json::Value>| {
                let sink = sink.clone();
                async move {
                    if let Some(t) = body.0.get("type").and_then(|v| v.as_str()) {
                        sink.lock().unwrap().push(t.to_string());
                    }
                    axum::http::StatusCode::OK
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let bus = Arc::new(EventBus::default());
        let (sd_tx, sd_rx) = tokio::sync::watch::channel(false);
        let handle = spawn_webhook_bridge(
            bus.clone(),
            sd_rx,
            Some(kb_core::config::WebhooksSection {
                url: format!("http://{addr}/hook"),
                types: vec!["artifact.indexed".into(), "comment.added".into()],
                timeout_ms: Some(2000),
                allow_private: false,
            }),
        )
        .expect("bridge spawns when configured");

        bus.emit(
            "artifact.indexed",
            serde_json::json!({"kb": "canon", "id": "a"}),
        );
        bus.emit("query", serde_json::json!({"kb": "canon"})); // not in allowlist
        bus.emit(
            "comment.added",
            serde_json::json!({"kb": "canon", "cid": "c1"}),
        );

        // Poll until both selected events land (or time out).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if received.lock().unwrap().len() >= 2 || tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let mut got = received.lock().unwrap().clone();
        got.sort();
        assert_eq!(
            got,
            vec!["artifact.indexed".to_string(), "comment.added".to_string()],
            "only allowlisted event types are forwarded (query dropped)"
        );

        // Shutdown flips → the task must observe the watch and return.
        sd_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("bridge task joins promptly on shutdown")
            .expect("task did not panic");

        server.abort();
    }

    // ---- CE4 — serve_loop decision helpers ----
    // (The full restart + rollback integration test lives in CE5, where
    // PUT /api/config provides the in-process restart trigger.)

    #[test]
    fn configs_equal_detects_changes() {
        let a = KbConfig::default();
        let mut b = KbConfig::default();
        assert!(configs_equal(&a, &b), "identical defaults compare equal");
        b.server.addr = "0.0.0.0:9999".into();
        assert!(!configs_equal(&a, &b), "an addr change is detected");
    }

    #[test]
    fn load_config_for_serve_defaults_on_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let c = load_config_for_serve(&tmp.path().join("nope.toml"));
        assert!(c.kb.is_empty());
        assert_eq!(c.server.addr, "127.0.0.1:4000", "falls back to defaults");
    }

    #[test]
    fn load_config_for_serve_reads_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb.toml");
        std::fs::write(&path, "[server]\naddr = \"127.0.0.1:4321\"\n").unwrap();
        assert_eq!(load_config_for_serve(&path).server.addr, "127.0.0.1:4321");
    }
}
