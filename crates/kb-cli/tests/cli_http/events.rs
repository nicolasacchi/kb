//! T1 (v0.24) — `kb events --follow` integration. Spawns the REAL `kb`
//! binary twice: once as a scratch daemon (KB_HOME-sandboxed, random
//! port), once as the follower with a server-side `--kb` filter. The
//! ring replay proves connect+print; a file dropped into the corpus
//! afterwards proves the LIVE tail (watcher → bus → SSE → stdout).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Kill-on-drop guard so a failing assert never leaks a process.
struct Proc {
    child: Child,
    stderr_path: PathBuf,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Proc {
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

/// Boot `kb daemon` on `port`, KB_HOME-sandboxed under `home`, with one
/// `smoke` kb whose corpus holds `first.html`.
fn spawn_daemon(home: &Path, port: u16) -> Proc {
    let corpus = home.join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(
        corpus.join("first.html"),
        "<!doctype html><html><head><title>first</title></head><body><h1>first</h1></body></html>",
    )
    .unwrap();
    let cfg_path = home.join("kb.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[daemon]\nname = \"events-smoke\"\n\n\
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
        .args(["daemon", "--config"])
        .arg(&cfg_path)
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .expect("spawn kb daemon");
    Proc { child, stderr_path }
}

/// Spawn the follower: `kb events --follow --json --kb smoke`, stdout
/// captured to a file the test polls.
fn spawn_follower(home: &Path, port: u16) -> (Proc, PathBuf) {
    let stdout_path = home.join("events-stdout.ndjson");
    let stderr_path = home.join("events-stderr.log");
    let child = Command::new(env!("CARGO_BIN_EXE_kb"))
        .env("KB_HOME", home)
        .env_remove("KB_STATE_DIR")
        .env_remove("KB_CONFIG_DIR")
        .env_remove("KB_CACHE_DIR")
        .args([
            "events",
            "--follow",
            "--json",
            "--kb",
            "smoke",
            "--daemon",
            &format!("http://127.0.0.1:{port}"),
        ])
        .stdout(std::fs::File::create(&stdout_path).unwrap())
        .stderr(std::fs::File::create(&stderr_path).unwrap())
        .spawn()
        .expect("spawn kb events");
    (Proc { child, stderr_path }, stdout_path)
}

/// Poll `stdout_path` until `pred` matches (or a process dies / 120 s).
fn wait_for_output(
    daemon: &mut Proc,
    follower: &mut Proc,
    stdout_path: &Path,
    what: &str,
    pred: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            panic!(
                "daemon exited early ({status}); stderr:\n{}",
                daemon.stderr()
            );
        }
        if let Some(status) = follower.child.try_wait().unwrap() {
            panic!(
                "follower exited early ({status}); stderr:\n{}",
                follower.stderr()
            );
        }
        let body = std::fs::read_to_string(stdout_path).unwrap_or_default();
        if pred(&body) {
            return body;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {what}; follower stdout:\n{body}\n\
                 follower stderr:\n{}\ndaemon stderr:\n{}",
                follower.stderr(),
                daemon.stderr()
            );
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The full T1 story end-to-end: the follower connects (retrying while
/// the daemon boots — the shared backoff loop), the ring replays the
/// initial-walk `artifact.indexed` through the server-side `kb:smoke`
/// filter, and a file written AFTER connect arrives live.
#[test]
fn events_follow_prints_ring_replay_then_live_events_as_ndjson() {
    let tmp = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut daemon = spawn_daemon(tmp.path(), port);
    let (mut follower, stdout_path) = spawn_follower(tmp.path(), port);

    // Ring replay: first.html's index event reaches the follower.
    let body = wait_for_output(
        &mut daemon,
        &mut follower,
        &stdout_path,
        "the ring-replayed artifact.indexed for first.html",
        |b| b.contains("artifact.indexed") && b.contains("first.html"),
    );
    // NDJSON contract: every complete line parses, carries type +
    // payload, and the server-side kb filter let only smoke through.
    let complete = body.rfind('\n').map(|i| &body[..i]).unwrap_or("");
    for line in complete.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("not ndjson ({e}): {line}"));
        assert!(v.get("type").is_some(), "no type field: {line}");
        let payload = v.get("payload").expect("no payload field");
        if let Some(kb) = payload.get("kb").and_then(|k| k.as_str()) {
            assert_eq!(kb, "smoke", "kb:smoke filter leaked: {line}");
        }
    }

    // Live tail: a file dropped in AFTER the follower connected flows
    // watcher → bus → SSE → stdout.
    std::fs::write(
        tmp.path().join("corpus").join("second.html"),
        "<!doctype html><html><head><title>second</title></head>\
         <body><h1>second</h1></body></html>",
    )
    .unwrap();
    wait_for_output(
        &mut daemon,
        &mut follower,
        &stdout_path,
        "the LIVE artifact.indexed for second.html",
        |b| b.contains("second.html"),
    );
}

#[test]
fn events_help_lists_follow_and_filter_flags() {
    let out = assert_cmd::Command::cargo_bin("kb")
        .unwrap()
        .args(["events", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    for flag in [
        "--follow",
        "--types",
        "--kb",
        "--artifact",
        "--json",
        "--daemon",
    ] {
        assert!(text.contains(flag), "missing {flag} in help:\n{text}");
    }
}

#[test]
fn events_without_follow_is_rejected() {
    // D7: the verb ships follow-only — a bare `kb events` must fail
    // loudly (clap required-flag error), not silently hang.
    assert_cmd::Command::cargo_bin("kb")
        .unwrap()
        .args(["events"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--follow"));
}
