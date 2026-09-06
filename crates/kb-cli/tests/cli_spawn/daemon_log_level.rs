//! L2 — `kb daemon log-level` against a real `kb daemon` process. The
//! spawned daemon installs the file-layer reload handle in its own
//! process (the in-process helpers never do), so this drives the whole
//! chain: CLI verb → PUT/GET `/api/log-level` → `reload::Handle` →
//! DEBUG lines actually landing in the ndjson file. Harness mirrors
//! `tests/daemon_logging.rs`.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DAEMON_NAME: &str = "loglevel-cli";

/// Kill-on-drop guard so a failing assert never leaks a daemon.
struct DaemonProc {
    child: Child,
    stderr_path: PathBuf,
}

impl Drop for DaemonProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl DaemonProc {
    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Boot `kb daemon` on a scratch port, KB_HOME-sandboxed under `home`.
/// Returns the proc guard + the port the daemon listens on.
fn spawn_daemon(home: &Path) -> (DaemonProc, u16) {
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
            "[daemon]\nname = \"{DAEMON_NAME}\"\n\n\
             [server]\naddr = \"127.0.0.1:{port}\"\n\n\
             [defaults]\ndisable_embedder_fallback = true\n\n\
             [kb.smoke]\npath = \"{}\"\n",
            corpus.display()
        ),
    )
    .unwrap();

    let stderr_path = home.join("daemon-stderr.log");
    let child = Command::new(env!("CARGO_BIN_EXE_kb"))
        .env("KB_HOME", home)
        .env_remove("KB_STATE_DIR")
        .env_remove("KB_CONFIG_DIR")
        .env_remove("KB_CACHE_DIR")
        .env_remove("RUST_LOG")
        .env_remove("KB_LOG_FILE_LEVEL")
        .args(["daemon", "--config"])
        .arg(&cfg_path)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .expect("spawn kb daemon");
    (DaemonProc { child, stderr_path }, port)
}

/// Concatenate every `kb.ndjson.*` file in the daemon's log dir.
fn read_log(home: &Path) -> String {
    let dir = home.join("state").join(DAEMON_NAME).join("log");
    let mut body = String::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            if entry.file_name().to_string_lossy().starts_with("kb.ndjson") {
                body.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
    }
    body
}

/// Poll until `pred(log)` (or the daemon dies / 120 s pass).
fn wait_for_log(daemon: &mut DaemonProc, home: &Path, what: &str, pred: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            panic!(
                "kb daemon exited early ({status}); stderr:\n{}",
                daemon.stderr()
            );
        }
        let body = read_log(home);
        if pred(&body) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; log so far:\n{body}\nstderr:\n{}",
            daemon.stderr()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Run `kb daemon log-level <args…>` sandboxed to `home`.
fn kb_log_level(home: &Path, endpoint: &str, args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = AssertCommand::cargo_bin("kb").unwrap();
    cmd.env("KB_HOME", home)
        .env_remove("KB_STATE_DIR")
        .env_remove("KB_CONFIG_DIR")
        .env_remove("KB_CACHE_DIR")
        .args(["daemon", "log-level"])
        .args(args)
        .args(["--endpoint", endpoint]);
    cmd.assert()
}

/// The full verb loop: read the boot default → set `debug` (and see
/// DEBUG lines land in the ndjson file — the flip is live, no restart)
/// → `--json` round-trips → bad directives fail with the daemon's 400.
#[test]
fn daemon_log_level_reads_sets_and_rejects() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let (mut daemon, port) = spawn_daemon(home);
    let endpoint = format!("http://127.0.0.1:{port}");

    // Daemon up + file logging live (the boot INFO line reached the file).
    wait_for_log(&mut daemon, home, "the boot INFO line", |b| {
        b.contains("kb-server listening")
    });
    assert!(
        !read_log(home).contains("\"level\":\"DEBUG\""),
        "info default: no DEBUG lines before the flip"
    );

    // GET — the boot default (info + the lance-scanner pin).
    let out = kb_log_level(home, &endpoint, &[]).success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("file log filter:") && stdout.contains("info"),
        "GET should print the current filter: {stdout}"
    );

    // PUT debug — echoes the new filter.
    let out = kb_log_level(home, &endpoint, &["debug"]).success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains("file log filter: debug"),
        "PUT should echo the new filter: {stdout}"
    );

    // The flip is live: further API traffic (the GET below) now writes
    // DEBUG lines into the ndjson file.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        kb_log_level(home, &endpoint, &[]).success();
        if read_log(home).contains("\"level\":\"DEBUG\"") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no DEBUG line landed after the flip; stderr:\n{}",
            daemon.stderr()
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    // --json round-trips the wire shape.
    let out = kb_log_level(home, &endpoint, &["--json"]).success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("--json output parses");
    assert_eq!(v["filter"], serde_json::json!("debug"));
    assert_eq!(v["installed"], serde_json::json!(true));

    // Bad directives → non-zero exit carrying the daemon's 400 detail.
    let out = kb_log_level(home, &endpoint, &["foo=bar"]).failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("invalid log filter"),
        "the 400 detail should surface: {stderr}"
    );
}
