//! L2 — runtime log level (`GET`/`PUT /api/log-level`) + log-file
//! retention. Three surfaces:
//!
//! - the route CONTRACT on a daemon whose process never ran
//!   `tracing_init::init` (the in-process helper): GET is total
//!   (`installed: false`), PUT is 409, bad directives are 400;
//! - the LIVE flip through the real kb-server binary (which installs the
//!   reload handle in `main`): PUT flips the ndjson file layer to
//!   `debug` without a restart, observed in the file itself;
//! - the boot-time retention sweep: an aged fabricated `kb.ndjson.*`
//!   file is pruned at boot while fresh + foreign files survive.
//!
//! Sweep-policy precision (prefix filter, cutoff edge, missing dir) is
//! unit-pinned in `kb_core::tracing_init`; harness mirrors
//! `tests/file_logging.rs` (the other spawned-binary suite).

use crate::common::{free_port, ServerProc};
use kb_core::config::KbConfig;
use kb_core::paths::KbPaths;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---------- in-process: route contract without file logging ----------

/// Zero-kb daemon via the in-process helper (which never initialises
/// tracing, so no reload handle exists in THIS test process — kb-core's
/// unit suite owns the installed-handle happy path for in-process use).
async fn boot_bare() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let daemon_name = format!("ll-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut cfg = KbConfig::default();
    cfg.daemon.name = Some(daemon_name.clone());
    cfg.defaults.disable_embedder_fallback = true;
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, addr)
}

#[tokio::test]
async fn log_level_route_contract_without_file_logging() {
    let (_tmp, addr) = boot_bare().await;
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/api/log-level");

    // GET is total: no handle → installed:false, filter:null.
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["installed"], serde_json::json!(false));
    assert!(body["filter"].is_null(), "no filter without init: {body}");
    assert_eq!(body["env"], serde_json::json!("KB_LOG_FILE_LEVEL"));

    // PUT without a handle → 409 problem+json.
    let resp = client
        .put(&url)
        .json(&serde_json::json!({ "filter": "debug" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("not initialised"),
        "409 detail should explain the missing init: {body}"
    );

    // Bad directives → 400, validated BEFORE the handle lookup.
    let resp = client
        .put(&url)
        .json(&serde_json::json!({ "filter": "foo=bar" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid log filter"),
        "400 detail should name the bad filter: {body}"
    );
}

// ---------- spawned binary: live flip + boot retention sweep ----------

/// Spawn the real kb-server binary (whose `main` runs `tracing_init::init`
/// — the in-process helpers never do), KB_HOME-sandboxed, on a scratch
/// port. Returns the proc guard + the port.
fn spawn_server(home: &Path, daemon_name: &str, extra_toml: &str) -> (ServerProc, u16) {
    let corpus = home.join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(
        corpus.join("smoke.html"),
        "<!doctype html><html><head><title>smoke</title></head><body><h1>smoke</h1></body></html>",
    )
    .unwrap();

    let port = free_port();
    let cfg_path = home.join("kb.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[daemon]\nname = \"{daemon_name}\"\n\n\
             [server]\naddr = \"127.0.0.1:{port}\"\n{extra_toml}\n\
             [defaults]\ndisable_embedder_fallback = true\n\n\
             [kb.smoke]\npath = \"{}\"\n",
            corpus.display()
        ),
    )
    .unwrap();

    let stderr_path = home.join("server-stderr.log");
    let child = Command::new(env!("CARGO_BIN_EXE_kb-server"))
        .env("KB_HOME", home)
        .env_remove("KB_STATE_DIR")
        .env_remove("KB_CONFIG_DIR")
        .env_remove("KB_CACHE_DIR")
        .env_remove("RUST_LOG")
        .env_remove("KB_LOG_FILE_LEVEL")
        .args(["--config"])
        .arg(&cfg_path)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .expect("spawn kb-server");
    (ServerProc { child, stderr_path }, port)
}

fn log_dir(home: &Path, daemon_name: &str) -> PathBuf {
    home.join("state").join(daemon_name).join("log")
}

/// Concatenate every `kb.ndjson.*` file in the daemon's log dir.
fn read_log(home: &Path, daemon_name: &str) -> String {
    let mut body = String::new();
    if let Ok(rd) = std::fs::read_dir(log_dir(home, daemon_name)) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().starts_with("kb.ndjson") {
                body.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
    }
    body
}

/// Poll until `pred` (or the server dies / 120 s pass).
fn wait_for(server: &mut ServerProc, what: &str, mut pred: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            panic!(
                "kb-server exited early ({status}); stderr:\n{}",
                server.stderr()
            );
        }
        if pred() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; stderr:\n{}",
            server.stderr()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// PUT /api/log-level against the live binary flips the ndjson file layer
/// to `debug` WITHOUT a restart: before the flip the file holds no
/// sub-info lines (L1 default), after it DEBUG lines land. GET reflects
/// the change.
#[tokio::test]
async fn log_level_put_flips_file_verbosity_live() {
    let tmp = tempfile::tempdir().unwrap();
    let name = "ll-flip";
    let (mut server, port) = spawn_server(tmp.path(), name, "");

    wait_for(&mut server, "the boot INFO line", || {
        read_log(tmp.path(), name).contains("kb-server listening")
    });
    assert!(
        !read_log(tmp.path(), name).contains("\"level\":\"DEBUG\""),
        "no DEBUG lines before the flip (info default)"
    );

    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/api/log-level");

    // GET → the L1 boot default, handle installed. `EnvFilter`'s Display
    // may reorder directives, so pin on containment, not the exact string.
    let body: serde_json::Value = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(body["installed"], serde_json::json!(true));
    let filter = body["filter"].as_str().unwrap_or_default();
    assert!(
        filter.contains("info") && filter.contains("lance::dataset::scanner=error"),
        "boot default missing from GET: {body}"
    );

    // PUT debug → 200 and the new filter echoes back.
    let resp = client
        .put(&url)
        .json(&serde_json::json!({ "filter": "debug" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["filter"], serde_json::json!("debug"));

    // The flip is live: request traffic now produces DEBUG lines in the
    // file (tower-http's request traces, among others). Poll while
    // generating traffic.
    wait_for(&mut server, "a DEBUG line after the flip", || {
        plain_http_get(&format!("http://127.0.0.1:{port}/api/kbs"));
        read_log(tmp.path(), name).contains("\"level\":\"DEBUG\"")
    });

    // GET agrees with what PUT installed.
    let body: serde_json::Value = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(body["filter"], serde_json::json!("debug"));

    // Bad directives still 400 on a live daemon, filter untouched.
    let resp = client
        .put(&url)
        .json(&serde_json::json!({ "filter": "foo=bar" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(body["filter"], serde_json::json!("debug"));
}

/// Tiny blocking GET used inside the sync poll closure (reqwest's async
/// client can't be awaited there; std TCP keeps the poll dependency-free).
fn plain_http_get(url: &str) {
    if let Some(hostpath) = url.strip_prefix("http://") {
        let (host, path) = hostpath.split_once('/').unwrap_or((hostpath, ""));
        if let Ok(mut s) = std::net::TcpStream::connect(host) {
            use std::io::{Read, Write};
            let _ = write!(s, "GET /{path} HTTP/1.0\r\nHost: {host}\r\n\r\n");
            let _ = s.flush();
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
        }
    }
}

/// Boot prune: an aged fabricated `kb.ndjson.*` file planted before boot
/// is deleted by the first sweep tick (default 14-day window); a foreign
/// file of the same age and the daemon's own fresh file survive.
#[test]
fn log_retention_prunes_aged_files_at_boot() {
    let tmp = tempfile::tempdir().unwrap();
    let name = "ll-prune";
    let dir = log_dir(tmp.path(), name);
    std::fs::create_dir_all(&dir).unwrap();

    let aged = dir.join("kb.ndjson.2020-01-01");
    let foreign = dir.join("keep.txt");
    std::fs::write(&aged, b"{\"old\":true}\n").unwrap();
    std::fs::write(&foreign, b"operator note\n").unwrap();
    let past = std::time::SystemTime::now() - Duration::from_secs(60 * 86_400);
    for p in [&aged, &foreign] {
        let f = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(past))
            .unwrap();
    }

    let (mut server, _port) = spawn_server(tmp.path(), name, "");

    wait_for(&mut server, "the aged log file to be pruned", || {
        !aged.exists()
    });
    assert!(foreign.exists(), "foreign files are never touched");
    // The daemon's own (fresh) ndjson file survives the sweep.
    wait_for(&mut server, "the daemon's own fresh log file", || {
        read_log(tmp.path(), name).contains("kb-server listening")
    });
}
