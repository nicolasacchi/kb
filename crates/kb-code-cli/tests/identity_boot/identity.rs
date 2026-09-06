//! `kb-code identity` — W1.2: a real HTTP client against a running
//! kb-code-server daemon. Boots the daemon in-process on a random port
//! (mirrors kb-cli's `serve_on_random_port_with_paths` + `assert_cmd`
//! combo, e.g. `crates/kb-cli/tests/comments.rs`), then shells out to the
//! `kb-code` binary pointed at it via `--daemon`.

use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use predicates::str::contains;

#[test]
fn version_flag_exits_zero() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}

async fn boot() -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "kb".to_string(),
            path: std::env::current_dir().unwrap(),
        }],
        ..KbCodeConfig::default()
    };
    // W1.5 — `bind_and_spawn` now opens a real kb-code store
    // (`<state>/index.db`), so tests must pass isolated `KbPaths` rather
    // than the bare `serve_on_random_port` (real XDG paths, would race
    // under parallel `cargo test` — same rationale as `kb_server`'s own
    // boot tests, e.g. `crates/kb-cli/tests/comments.rs::boot`). The
    // `TempDir` is returned (not dropped here) so it outlives the daemon
    // task the caller is about to run requests against.
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

// multi_thread + ≥2 worker threads: `boot()` spawns the daemon's serve
// task onto this SAME runtime, and `assert_cmd`'s `.assert()` below
// synchronously blocks a worker thread waiting on the `kb-code` child
// process. On the default single-threaded `#[tokio::test]` flavor that
// blocks the daemon's own task from ever running, so the child's HTTP GET
// times out — mirrors kb-cli's `list_and_export_round_trip_against_daemon`
// (crates/kb-cli/tests/comments.rs), which hit the same trap first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_json_flag_prints_daemon_identity() {
    let (_tmp, url, task) = boot().await;
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["identity", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    assert_eq!(body["name"].as_str(), Some("kb-code"));
    assert!(body["version"].as_str().is_some());
    assert!(body["started_at"].as_str().is_some());
    let repos = body["repos"].as_array().expect("repos array");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["name"].as_str(), Some("kb"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_default_output_is_human_readable() {
    let (_tmp, url, task) = boot().await;
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["identity", "--daemon", &url])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("name:"), "got: {text}");
    assert!(text.contains("kb-code"), "got: {text}");
    assert!(text.contains("version:"), "got: {text}");
    assert!(text.contains("repos:"), "got: {text}");
    assert!(text.contains("kb ("), "expected the repo line; got: {text}");
    task.abort();
}

#[test]
fn identity_unreachable_daemon_fails_cleanly() {
    // Port 0 is never a live listener to connect to — a clean connection
    // error, not a panic/backtrace.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["identity", "--daemon", "http://127.0.0.1:0"])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}
