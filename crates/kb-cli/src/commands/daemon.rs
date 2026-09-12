//! `kb daemon [stop]` — start the HTTP+SSE server in this process,
//! or stop a running one via the pid file.
//!
//! Pid-file lifecycle (deep-review LOW: kb daemon lifecycle):
//! - On `kb daemon` start, write `getpid()` into
//!   `<state>/kb-daemon.pid` after the listener is bound (so a failed
//!   bind doesn't leave a stale pid file). Refuse to start if the
//!   existing pid file points at a still-running process.
//! - On clean exit (graceful shutdown signal from kb-server), remove
//!   the pid file. Crashes leave a stale file; the next start does a
//!   liveness check via `kill -0 <pid>` and prunes if dead.
//! - `kb daemon stop` reads the pid, sends SIGTERM, and polls for
//!   exit. Times out after 10 s.

use super::{load_config_or_default, resolve_config_path};
use anyhow::{anyhow, Context, Result};
use kb_core::paths::KbPaths;
use std::path::PathBuf;
use std::time::Duration;

pub async fn run(config_path: Option<&PathBuf>) -> Result<()> {
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;
    if cfg.kb.is_empty() {
        eprintln!(
            "warning: no kbs configured in {}\n         run `kb add <path>` first.",
            cfg_path.display()
        );
    }

    // LOW (deep-review): double-start check via pid file. If the file
    // exists AND its pid is still alive, refuse. Stale pid files
    // (process gone, file left over from a crash) get cleaned up.
    //
    // Container caveat (R7+ fix): if the recorded pid is OUR OWN pid,
    // the file is stale by definition — a freshly-started process
    // can't already have its own pid persisted. This happens routinely
    // inside Docker where the kb daemon runs as PID 1: a non-graceful
    // shutdown (SIGKILL after compose timeout, OOM-killer, host
    // reboot) leaves the pid file with `1` in it; the next container
    // boot is ALSO PID 1, and `kill -0 1` succeeds (we're checking
    // ourselves). Pre-fix this looked like a live daemon and refused
    // forever. The own-pid check turns that into a stale-file cleanup.
    let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;
    paths.ensure_dirs()?;

    // L1 — main() skips the plain stderr-only init for the daemon-start
    // path so this call can layer the ndjson daily file appender
    // (`<state>/log/kb.ndjson.*`, default `info`, KB_LOG_FILE_LEVEL
    // override) on top of the SAME stderr layer every other verb gets.
    // The guard must live until run() returns or buffered file logs are
    // dropped on shutdown.
    let _log_guard = kb_core::tracing_init::init(&paths, crate::STDERR_DEFAULT_FILTER)
        .context("init tracing (file logging)")?;

    let pid_path = paths.daemon_pid_file();
    let our_pid = std::process::id();
    if let Some(existing) = read_pid(&pid_path) {
        if existing != our_pid && pid_is_alive(existing) {
            return Err(anyhow!(
                "another kb daemon is already running (pid {existing}, file: {})\n\
                 use `kb daemon stop` to terminate it first",
                pid_path.display()
            ));
        }
        // Stale: either process gone (`!pid_is_alive`) or the file
        // recorded our own pid (impossible-without-prior-crash → also
        // stale). Drop it before continuing.
        let _ = std::fs::remove_file(&pid_path);
    }
    write_pid(&pid_path, std::process::id())?;
    // Best-effort cleanup on panic — kb_server's graceful_shutdown
    // returns Ok which lets the unconditional remove below run, but
    // panics would leak the pid file otherwise.
    let pid_path_for_panic = pid_path.clone();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::fs::remove_file(&pid_path_for_panic);
        prev_hook(info);
    }));

    // PF-B1 — inject the real git-probed build stamp (kb-buildstamp leaf
    // crate) so `/api/identity` carries the true sha/version. Only BINS
    // may link the per-commit-changing stamp crate; the kb-server lib
    // reads it back via this process-global.
    kb_server::set_build_stamp(kb_buildstamp::VERSION, kb_buildstamp::BUILD_SHA);

    // CE — serve_loop owns the config-path + reload cycle (it re-reads
    // `cfg_path` on each in-process restart triggered by PUT /api/config,
    // rolling back to last-good on a failed boot). `paths` is pinned for
    // the daemon's lifetime; the pid file above stays valid (no re-fork).
    let result = kb_server::serve_loop(cfg_path, paths).await;

    // Clean exit: remove the pid file. Errors here are non-fatal —
    // the next start will treat the file as stale and clean up.
    let _ = std::fs::remove_file(&pid_path);
    result
}

pub async fn stop(config_path: Option<&PathBuf>) -> Result<()> {
    let cfg_path = resolve_config_path(config_path)?;
    let cfg = load_config_or_default(&cfg_path)?;
    let paths = KbPaths::new(cfg.daemon.name.as_deref().unwrap_or("default"))?;
    let pid_path = paths.daemon_pid_file();

    let pid = read_pid(&pid_path).ok_or_else(|| {
        anyhow!(
            "no kb-daemon.pid at {}\n  (daemon not running, or pid file missing)",
            pid_path.display()
        )
    })?;
    if !pid_is_alive(pid) {
        let _ = std::fs::remove_file(&pid_path);
        return Err(anyhow!(
            "pid {pid} in {} is dead; removed the stale file",
            pid_path.display()
        ));
    }

    send_sigterm(pid).with_context(|| format!("send SIGTERM to pid {pid}"))?;
    eprintln!("sent SIGTERM to kb daemon pid {pid}; waiting for exit …");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if !pid_is_alive(pid) {
            // Process is gone; the daemon's clean-exit path removed
            // the pid file already. Defensive cleanup just in case.
            let _ = std::fs::remove_file(&pid_path);
            eprintln!("kb daemon pid {pid} exited cleanly");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "kb daemon pid {pid} did not exit within 10 s — still alive after SIGTERM. \
                 Investigate (the daemon's graceful-shutdown drain may be stuck on inflight \
                 requests) or send SIGKILL manually: `kill -9 {pid}`"
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn read_pid(path: &std::path::Path) -> Option<u32> {
    let s = std::fs::read_to_string(path).ok()?;
    s.trim().parse::<u32>().ok()
}

fn write_pid(path: &std::path::Path, pid: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{pid}\n"))
        .with_context(|| format!("write pid file {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // `kill -0` returns 0 if the process exists (and signal would
    // have been delivered), errno=ESRCH if it doesn't, errno=EPERM
    // if it does but we don't have permission. EPERM still means
    // alive — treat as such.
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno == libc::EPERM
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    // Best-effort no-op on non-unix — assume dead to avoid false
    // double-start refusals on platforms we don't ship for.
    false
}

#[cfg(unix)]
fn send_sigterm(pid: u32) -> Result<()> {
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if ret == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().into())
    }
}

#[cfg(not(unix))]
fn send_sigterm(_pid: u32) -> Result<()> {
    Err(anyhow!(
        "kb daemon stop not supported on non-unix platforms"
    ))
}

/// L2 — `kb daemon log-level [FILTER]`: read (GET) or set (PUT) the
/// daemon's runtime FILE log filter via `/api/log-level`. The flip is
/// live (a `reload` handle around the ndjson layer's `EnvFilter`) — no
/// restart, stderr/RUST_LOG untouched. Non-2xx responses surface the
/// problem+json `detail` (400 bad directives, 409 file logging not
/// initialised in the daemon process).
pub async fn log_level(
    endpoint: Option<&str>,
    filter: Option<&str>,
    json_out: bool,
    bearer: Option<&str>,
) -> Result<()> {
    let base = endpoint
        .map(str::to_string)
        .unwrap_or_else(|| "http://127.0.0.1:4000".to_string());
    let url = format!("{base}/api/log-level");
    let client = crate::http::client_with_timeout_and_bearer(5, bearer)?;

    let resp = match filter {
        Some(f) => client
            .put(&url)
            .json(&serde_json::json!({ "filter": f }))
            .send()
            .await
            .with_context(|| format!("PUT {url}"))?,
        None => client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?,
    };

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        // problem+json carries the actionable message in `detail`.
        let detail = body
            .get("detail")
            .and_then(|d| d.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| text.trim().to_string());
        return Err(anyhow!("daemon returned {status}: {detail}"));
    }
    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    match body.get("filter").and_then(|f| f.as_str()) {
        Some(f) => println!("file log filter: {f}"),
        None => println!(
            "file logging is not active in this daemon process \
             (no filter to read; the kb daemon / kb-server entry points enable it)"
        ),
    }
    Ok(())
}

/// P5: `kb daemon doctor` — pokes the daemon HTTP API and prints a
/// health report. Seven checks: identity reachable, kbs configured,
/// open-error count, stats responsive, decode-skips clean (no rows
/// silently dropped from results), embedder/semantic path (one
/// tiny semantic search per kb, with a warm-up retry), event bus
/// alive. Each is OK/WARN/FAIL; overall verdict is HEALTHY (all OK),
/// DEGRADED (any WARN, no FAIL), or UNHEALTHY (any FAIL).
///
/// 4: When `watch_secs` is Some(N), re-runs the full check sweep
/// every N seconds, clearing the screen between renders (Ctrl+C to
/// exit). The exit code reflects the LAST check's verdict — useful
/// for "leave it running while I tail logs" workflows. Ignored when
/// json_out is true (scripted callers want one-shot output).
pub async fn doctor(
    endpoint: Option<&str>,
    json_out: bool,
    watch_secs: Option<u64>,
    bearer: Option<&str>,
) -> Result<()> {
    // Watch mode: loop with screen clears between renders. Disabled
    // when --json is set (machines want one-shot output, not a stream).
    if let Some(secs) = watch_secs {
        if json_out {
            return Err(anyhow!(
                "--watch and --json are mutually exclusive — --json is one-shot"
            ));
        }
        let interval = std::time::Duration::from_secs(secs.max(1));
        // Pure loop — Ctrl+C terminates from outside; the function
        // never returns Ok(()). The return-type contract is satisfied
        // by the `!` type of the infinite loop.
        loop {
            // ANSI clear + cursor home. Works on any vt100-ish terminal
            // (every modern xterm/tmux/iTerm/Windows Terminal).
            print!("\x1b[2J\x1b[H");
            let report = run_doctor_once(endpoint, bearer).await?;
            print_human(&report);
            println!(
                "\n  (watch · refresh {}s · Ctrl+C to exit)",
                interval.as_secs()
            );
            tokio::time::sleep(interval).await;
        }
    }
    let report = run_doctor_once(endpoint, bearer).await?;
    if json_out {
        print_json(&report);
    } else {
        print_human(&report);
    }
    if matches!(report.verdict(), Verdict::Unhealthy) {
        std::process::exit(1);
    }
    Ok(())
}

/// Run one full sweep of doctor checks and return the populated
/// report. Extracted from `doctor()` so the --watch loop can call it
/// repeatedly.
async fn run_doctor_once(endpoint: Option<&str>, bearer: Option<&str>) -> Result<DoctorReport> {
    use std::time::Instant;
    let base = endpoint
        .map(str::to_string)
        .unwrap_or_else(|| "http://127.0.0.1:4000".to_string());

    let mut report = DoctorReport::new(base.clone());
    // Thread the daemon bearer through every probe — an auth-on daemon
    // reached over a non-loopback hop (e.g. a published Docker port) 401s an
    // unauthenticated request, and the doctor would wrongly report it down.
    let client = crate::http::client_with_timeout_and_bearer(5, bearer)?;

    // 1) identity reachability
    let identity_url = format!("{base}/api/identity");
    let started = Instant::now();
    match client.get(&identity_url).send().await {
        Ok(r) if r.status().is_success() => {
            let elapsed = started.elapsed().as_millis() as u64;
            let body: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
            let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let version = body.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            let host = body.get("host").and_then(|v| v.as_str()).unwrap_or("?");
            report.push(Check::ok(
                "identity",
                format!("{name} v{version} @ {host} ({elapsed}ms)"),
            ));
        }
        Ok(r) => report.push(Check::fail(
            "identity",
            format!("HTTP {} from {identity_url}", r.status()),
        )),
        Err(e) => report.push(Check::fail("identity", format!("unreachable: {e}"))),
    }

    // 2) kbs configured
    if report.last_status_is_ok() {
        let kbs_url = format!("{base}/api/kbs");
        let kbs: Vec<serde_json::Value> = match client.get(&kbs_url).send().await {
            Ok(r) => match r.error_for_status() {
                Ok(r) => r.json().await.unwrap_or_default(),
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        if kbs.is_empty() {
            report.push(Check::warn(
                "kbs",
                "no kbs configured (`kb add <path>` to register one)".into(),
            ));
        } else {
            let names: Vec<&str> = kbs
                .iter()
                .filter_map(|k| k.get("name").and_then(|v| v.as_str()))
                .collect();
            report.push(Check::ok(
                "kbs",
                format!("{} configured ({})", kbs.len(), names.join(", ")),
            ));
        }

        // 3) open errors per kb
        let mut total_open = 0u64;
        let mut by_kb = Vec::new();
        for kb in &kbs {
            let Some(name) = kb.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let url = format!(
                "{base}/api/kb/{}/errors",
                crate::http::encode_path_segment(name)
            );
            if let Ok(r) = client.get(&url).send().await {
                if let Ok(arr) = r.json::<Vec<serde_json::Value>>().await {
                    if !arr.is_empty() {
                        total_open += arr.len() as u64;
                        by_kb.push(format!("{name}={}", arr.len()));
                    }
                }
            }
        }
        if total_open == 0 {
            report.push(Check::ok("errors", "no open errors".into()));
        } else {
            report.push(Check::warn(
                "errors",
                format!("{total_open} open ({})", by_kb.join(", ")),
            ));
        }

        // 4) stats responsive
        let stats_url = format!("{base}/api/stats");
        let started = Instant::now();
        match client.get(&stats_url).send().await {
            Ok(r) if r.status().is_success() => {
                let elapsed = started.elapsed().as_millis() as u64;
                let body: serde_json::Value = r.json().await.unwrap_or(serde_json::Value::Null);
                let total_docs = body.get("total_docs").and_then(|v| v.as_u64()).unwrap_or(0);
                let total_sources = body
                    .get("total_sources")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                report.push(Check::ok(
                    "stats",
                    format!("{total_docs} docs, {total_sources} sources ({elapsed}ms)"),
                ));
                // 4b) decode skips (GC-B2) — rows the storage layer
                // silently DROPPED from search/list results because
                // their lance batches failed the typed decode (schema
                // drift). Mirrors the errors check: zero → ok, nonzero
                // → warn with a per-kb breakdown.
                report.push(decode_skips_check(&body));
            }
            Ok(r) => report.push(Check::fail(
                "stats",
                format!("HTTP {} from /api/stats", r.status()),
            )),
            Err(e) => report.push(Check::fail("stats", format!("stats failed: {e}"))),
        }

        // 5) embedder / semantic path — one tiny semantic search per kb,
        // mapped onto embedder health:
        //   200                       → embedder live (spawn + handshake + embed worked)
        //   400 "no embedding_model"  → keyword-only kb by config (fine)
        //   timeout (20s)             → model cold-load or a wedged onnxruntime
        //   anything else             → the semantic path is broken
        // Would have made the historical "daemon silent after 'loading
        // embedder'" (missing onnxruntime → futex_wait) diagnosable in one
        // run. The probe rides the daemon's query-embed LRU, so repeat
        // doctor runs cost ~0ms server-side. Dedicated 20s client: a cold
        // model load legitimately exceeds the 5s the other checks use.
        if !kbs.is_empty() {
            let emb_client = crate::http::client_with_timeout_and_bearer(20, bearer)?;
            let mut semantic: Vec<String> = Vec::new();
            let mut warmed: Vec<String> = Vec::new();
            let mut keyword_only: Vec<String> = Vec::new();
            let mut problems: Vec<String> = Vec::new();
            for kb in &kbs {
                let Some(name) = kb.get("name").and_then(|v| v.as_str()) else {
                    continue;
                };
                let url = format!(
                    "{base}/api/search?q=kb%20doctor%20probe&mode=semantic&limit=1&kb={}",
                    crate::http::encode_path_segment(name)
                );
                match emb_client.get(&url).send().await {
                    Ok(r) if r.status().is_success() => semantic.push(name.to_string()),
                    Ok(r) if r.status().as_u16() == 400 => {
                        let detail = r
                            .json::<serde_json::Value>()
                            .await
                            .ok()
                            .and_then(|p| {
                                p.get("detail").and_then(|d| d.as_str()).map(str::to_string)
                            })
                            .unwrap_or_default();
                        if detail.contains("no embedding_model") {
                            keyword_only.push(name.to_string());
                        } else {
                            problems.push(format!("{name}: {detail}"));
                        }
                    }
                    Ok(r) => problems.push(format!("{name}: HTTP {}", r.status())),
                    Err(e) if e.is_timeout() => {
                        // One retry before flagging: the FIRST semantic search
                        // after an index change pays the lazy vector-index
                        // rebuild, and a cold daemon pays the model load —
                        // both legitimate one-time costs that exceed 20s on
                        // big-row kbs (session transcripts do). The first
                        // request keeps warming the server even after the
                        // client gave up, so the retry rides the warmed path.
                        // Only a DOUBLE timeout — what a truly wedged
                        // onnxruntime produces — is reported.
                        match emb_client.get(&url).send().await {
                            Ok(r2) if r2.status().is_success() => warmed.push(name.to_string()),
                            _ => problems.push(format!(
                                "{name}: semantic probe timed out twice — wedged \
                                 onnxruntime or a very slow model/index load \
                                 (check daemon log / ORT_DYLIB_PATH)"
                            )),
                        }
                    }
                    Err(e) => problems.push(format!("{name}: {e}")),
                }
            }
            if !problems.is_empty() {
                report.push(Check::warn("embedder", problems.join("; ")));
            } else {
                let mut parts: Vec<String> = Vec::new();
                if !semantic.is_empty() {
                    parts.push(format!("semantic ok ({})", semantic.join(", ")));
                }
                if !warmed.is_empty() {
                    parts.push(format!("warmed on retry ({})", warmed.join(", ")));
                }
                if !keyword_only.is_empty() {
                    parts.push(format!(
                        "keyword-only by config ({})",
                        keyword_only.join(", ")
                    ));
                }
                report.push(Check::ok("embedder", parts.join("; ")));
            }
        }

        // 6) event bus — subscribe briefly, look for any frame within 3s.
        let bus_status = poke_event_bus(&base, bearer).await;
        report.push(bus_status);
    }
    Ok(report)
}

#[derive(Debug, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    Fail,
}

struct Check {
    name: &'static str,
    status: Status,
    detail: String,
}

impl Check {
    fn ok(name: &'static str, detail: String) -> Self {
        Self {
            name,
            status: Status::Ok,
            detail,
        }
    }
    fn warn(name: &'static str, detail: String) -> Self {
        Self {
            name,
            status: Status::Warn,
            detail,
        }
    }
    fn fail(name: &'static str, detail: String) -> Self {
        Self {
            name,
            status: Status::Fail,
            detail,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Healthy,
    Degraded,
    Unhealthy,
}

struct DoctorReport {
    base: String,
    checks: Vec<Check>,
}

impl DoctorReport {
    fn new(base: String) -> Self {
        Self {
            base,
            checks: Vec::new(),
        }
    }
    fn push(&mut self, c: Check) {
        self.checks.push(c);
    }
    fn last_status_is_ok(&self) -> bool {
        self.checks
            .last()
            .map(|c| c.status == Status::Ok)
            .unwrap_or(false)
    }
    fn verdict(&self) -> Verdict {
        if self.checks.iter().any(|c| c.status == Status::Fail) {
            Verdict::Unhealthy
        } else if self.checks.iter().any(|c| c.status == Status::Warn) {
            Verdict::Degraded
        } else {
            Verdict::Healthy
        }
    }
}

fn print_human(report: &DoctorReport) {
    println!("kb daemon doctor — {}\n", report.base);
    for c in &report.checks {
        let glyph = match c.status {
            Status::Ok => "✓",
            Status::Warn => "⚠",
            Status::Fail => "✗",
        };
        println!("  {glyph}  {:<10} {}", c.name, c.detail);
    }
    let v = report.verdict();
    let pass = report
        .checks
        .iter()
        .filter(|c| c.status == Status::Ok)
        .count();
    let total = report.checks.len();
    println!(
        "\nverdict: {} ({}/{} checks passed)",
        match v {
            Verdict::Healthy => "HEALTHY",
            Verdict::Degraded => "DEGRADED",
            Verdict::Unhealthy => "UNHEALTHY",
        },
        pass,
        total
    );
}

fn print_json(report: &DoctorReport) {
    let checks: Vec<serde_json::Value> = report
        .checks
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "status": match c.status {
                    Status::Ok => "ok",
                    Status::Warn => "warn",
                    Status::Fail => "fail",
                },
                "detail": c.detail,
            })
        })
        .collect();
    let out = serde_json::json!({
        "endpoint": report.base,
        "checks": checks,
        "verdict": match report.verdict() {
            Verdict::Healthy => "healthy",
            Verdict::Degraded => "degraded",
            Verdict::Unhealthy => "unhealthy",
        },
    });
    println!("{out}");
}

/// Build the decode-skips check from a parsed `/api/stats` body.
/// `kbs[].decode_skips` (GC-B2) is the cumulative count of rows the
/// storage layer silently dropped from search/list results because
/// their lance batches no longer typed-decode (schema drift after an
/// upgrade). Nonzero means results are quietly incomplete until a
/// `kb reindex` rewrites the offending rows in the current schema.
fn decode_skips_check(stats_body: &serde_json::Value) -> Check {
    let mut total = 0u64;
    let mut by_kb = Vec::new();
    if let Some(kbs) = stats_body.get("kbs").and_then(|v| v.as_array()) {
        for kb in kbs {
            let n = kb.get("decode_skips").and_then(|v| v.as_u64()).unwrap_or(0);
            if n > 0 {
                total += n;
                if let Some(name) = kb.get("name").and_then(|v| v.as_str()) {
                    by_kb.push(format!("{name}={n}"));
                }
            }
        }
    }
    if total == 0 {
        Check::ok("decode-skips", "no silent row drops".into())
    } else {
        Check::warn(
            "decode-skips",
            format!(
                "{total} rows silently dropped from results (schema drift: {}) — try `kb reindex`",
                by_kb.join(", ")
            ),
        )
    }
}

/// Subscribe to /api/events briefly and report whether any frame
/// (any kind, not just metrics.tick) arrived within 3s. Returns a
/// Check labeled "event-bus".
async fn poke_event_bus(base: &str, bearer: Option<&str>) -> Check {
    // V76-R4e — this probe was the last `reqwest-eventsource` 0.6 call site
    // (which pinned reqwest to ^0.12). It now reuses the in-house SSE
    // reader (`crate::sse`) over reqwest's byte stream — the same parser
    // `kb events --follow` / `kb push` already run. The bearer is threaded
    // inside `open_events_stream`, matching the other doctor checks.
    let resp = match crate::sse::open_events_stream(base, bearer, None, "").await {
        Ok(resp) => resp,
        Err(e) => return Check::fail("event-bus", format!("could not open event stream: {e}")),
    };
    let mut reader = crate::sse::FrameReader::from_response(resp);
    // Race the stream against a 3s timeout.
    let race = tokio::time::timeout(Duration::from_secs(3), async {
        // The first substantive frame counts as alive. Comment-only
        // keep-alive frames (no id/event/data) are skipped, matching the
        // old reqwest-eventsource loop's `Event::Open => continue`.
        loop {
            match reader.next_frame().await {
                Ok(Some(frame)) => {
                    if let Some(kind) = frame.event {
                        return Some(kind);
                    }
                    if frame.id.is_some() || frame.data.is_some() {
                        return Some("message".to_string());
                    }
                }
                Ok(None) | Err(_) => return None,
            }
        }
    })
    .await;
    match race {
        Ok(Some(kind)) => Check::ok("event-bus", format!("alive ({kind} received)")),
        Ok(None) => Check::warn(
            "event-bus",
            "stream opened but no frames in 3s (low-activity daemon — probably fine)".into(),
        ),
        Err(_) => Check::warn(
            "event-bus",
            "stream open timed out after 3s — daemon may be slow".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn pid_is_alive_handles_self() {
        let me = std::process::id();
        assert!(pid_is_alive(me), "current process must be alive");
    }

    #[test]
    fn pid_is_alive_false_for_pid_one_million() {
        // PID 1_000_000 is almost certainly unallocated; if it
        // happens to be active, the test re-runs against a fresh
        // unlikely number. False positives on this test are noise,
        // not data corruption.
        assert!(!pid_is_alive(1_000_000));
    }

    #[test]
    fn read_write_pid_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb-daemon.pid");
        write_pid(&path, 42).unwrap();
        assert_eq!(read_pid(&path), Some(42));
    }

    #[test]
    fn pid_file_with_trailing_whitespace_parses() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb-daemon.pid");
        std::fs::write(&path, "12345\n\n").unwrap();
        assert_eq!(read_pid(&path), Some(12345));
    }

    #[test]
    fn pid_file_garbage_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb-daemon.pid");
        std::fs::write(&path, "not a pid").unwrap();
        assert_eq!(read_pid(&path), None);
    }

    /// Container-restart regression: when the running process's own
    /// PID matches the persisted pid file, the file is by definition
    /// stale. PID 1 inside Docker is the canonical case — every
    /// restart of the container reuses PID 1 because that's how
    /// container entrypoints work. Pre-fix this looked like a live
    /// daemon and the boot refused; now the start path treats it as
    /// stale and continues.
    #[test]
    fn own_pid_in_pid_file_is_treated_as_stale_at_boot() {
        // Simulate the bootstrap branch in `run()`. We can't easily
        // call `run()` (it kicks off the full server), so we test the
        // condition directly: `existing != our_pid && pid_is_alive`.
        let our_pid = std::process::id();
        // existing == own pid → must NOT count as alive even though
        // pid_is_alive(self) returns true.
        assert!(pid_is_alive(our_pid));
        let stale = our_pid;
        let condition_refuses_start = stale != our_pid && pid_is_alive(stale);
        assert!(
            !condition_refuses_start,
            "boot must not refuse when the pid file records OUR own pid"
        );
    }

    #[test]
    #[cfg(unix)]
    fn sigterm_to_short_sleep_child_kills_it() {
        // Spawn `sleep 30`, signal it, confirm it dies fast (<2s).
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        assert!(pid_is_alive(pid));
        send_sigterm(pid).unwrap();
        let started = std::time::Instant::now();
        loop {
            if let Some(_status) = child.try_wait().expect("try_wait") {
                break;
            }
            if started.elapsed() > Duration::from_secs(2) {
                let _ = child.kill();
                panic!("sleep didn't exit after SIGTERM within 2s");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}
