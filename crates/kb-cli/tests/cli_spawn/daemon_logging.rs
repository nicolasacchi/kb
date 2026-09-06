//! L1 — the `kb daemon` path writes ndjson logs under `<state>/log/`.
//!
//! The tracing wiring lives in the binary entry points (main dispatch +
//! `commands::daemon::run`); the in-process `serve_on_random_port` boots
//! other suites use bypass it entirely. So these tests spawn the REAL
//! `kb` binary with `KB_HOME` pointing at a tempdir and a scratch port,
//! then watch the log dir. Level-gating precision (info passes / debug
//! dropped by the same layer) is unit-pinned in `kb_core::tracing_init`;
//! here we prove the end-to-end wiring: file appears, lines are JSON,
//! the default is info, and `KB_LOG_FILE_LEVEL` overrides it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
fn spawn_daemon(home: &Path, extra_env: &[(&str, &str)]) -> DaemonProc {
    let corpus = home.join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(
        corpus.join("smoke.html"),
        "<!doctype html><html><head><title>smoke</title></head><body><h1>smoke</h1></body></html>",
    )
    .unwrap();

    let cfg_path = home.join("kb.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[daemon]\nname = \"log-smoke\"\n\n\
             [server]\naddr = \"127.0.0.1:{}\"\n\n\
             [defaults]\ndisable_embedder_fallback = true\n\n\
             [kb.smoke]\npath = \"{}\"\n",
            free_port(),
            corpus.display()
        ),
    )
    .unwrap();

    let stderr_path = home.join("daemon-stderr.log");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_kb"));
    cmd.env("KB_HOME", home)
        .env_remove("KB_STATE_DIR")
        .env_remove("KB_CONFIG_DIR")
        .env_remove("KB_CACHE_DIR")
        .env_remove("RUST_LOG")
        .env_remove("KB_LOG_FILE_LEVEL")
        .args(["daemon", "--config"])
        .arg(&cfg_path)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn kb daemon");
    DaemonProc { child, stderr_path }
}

/// Concatenate every `kb.ndjson.*` file in the daemon's log dir.
fn read_log(home: &Path) -> String {
    let dir = home.join("state").join("log-smoke").join("log");
    let mut body = String::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("kb.ndjson") {
                body.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
    }
    body
}

/// Poll the log dir until `pred` matches (or the daemon dies / 120 s pass).
fn wait_for_log(
    daemon: &mut DaemonProc,
    home: &Path,
    what: &str,
    pred: fn(&str) -> bool,
) -> String {
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
            return body;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {what}; log so far:\n{body}\nstderr:\n{}",
                daemon.stderr()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Complete lines only — the daemon may be mid-write on the last one.
fn complete_lines(body: &str) -> impl Iterator<Item = &str> {
    let upto = body.rfind('\n').map(|i| &body[..i]).unwrap_or("");
    upto.lines().filter(|l| !l.trim().is_empty())
}

/// Default file level is info: the boot INFO lines land as parseable
/// ndjson, and nothing below info leaks into the file.
#[test]
fn daemon_writes_ndjson_log_file_at_info_default() {
    let tmp = tempfile::tempdir().unwrap();
    let mut daemon = spawn_daemon(tmp.path(), &[]);

    let body = wait_for_log(&mut daemon, tmp.path(), "the boot INFO line", |b| {
        b.contains("kb-server listening")
    });

    let mut saw_info = false;
    for line in complete_lines(&body) {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not ndjson ({e}): {line}"));
        assert!(v.get("timestamp").is_some(), "no timestamp field: {line}");
        let level = v["level"].as_str().unwrap_or_default().to_string();
        assert!(
            level != "DEBUG" && level != "TRACE",
            "sub-info line leaked past the default info filter: {line}"
        );
        if level == "INFO" {
            saw_info = true;
        }
    }
    assert!(saw_info, "no INFO line in the log file:\n{body}");
}

/// `KB_LOG_FILE_LEVEL=debug` opens the file layer up: the indexer's boot
/// DEBUG events (content-hash cache pre-population / `indexer recv`)
/// reach the file, proving the env override is honored end-to-end.
#[test]
fn kb_log_file_level_env_overrides_the_file_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let mut daemon = spawn_daemon(tmp.path(), &[("KB_LOG_FILE_LEVEL", "debug")]);

    let body = wait_for_log(&mut daemon, tmp.path(), "a DEBUG-level line", |b| {
        b.contains("\"level\":\"DEBUG\"")
    });

    // Still ndjson at the wider level — each complete line stays parseable.
    for line in complete_lines(&body) {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not ndjson ({e}): {line}"));
        assert!(v.get("level").is_some(), "no level field: {line}");
    }
}
