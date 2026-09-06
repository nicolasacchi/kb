//! `kb-code identity` — W1.2: a real HTTP client against a running
//! kb-code-server daemon. Boots the daemon in-process on a random port
//! (mirrors kb-cli's `serve_on_random_port_with_paths` + `assert_cmd`
//! combo, e.g. `crates/kb-cli/tests/comments.rs`), then shells out to the
//! `kb-code` binary pointed at it via `--daemon`.

use crate::common;
use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn version_flag_exits_zero() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .arg("--version")
        .assert()
        .success();
}

/// V71-X1 — this used to configure the repo as `std::env::current_dir()`,
/// which under `cargo test` IS THE WHOLE kb CHECKOUT: `bind_and_spawn`'s
/// background index then walked and hashed every file in this repo on
/// every single test run (measured: ~16 min wall, ~7.5 GB written per
/// `cargo test -p kb-code-cli --test identity_boot` invocation), for tests
/// that only assert on the `repos` array's `name` field and never read a
/// file or a symbol count. A tiny two-file git fixture (the SAME shape
/// `daemon.rs::fixture_repo` already uses, kept local per this test
/// binary's `common/mod.rs` doc convention — "divergent `fixture_repo`
/// implementations stay next to the tests that own them") gives the
/// daemon a real, near-instant-to-index repo instead.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    common::git(dir, &["init", "-q", "-b", "main"]);
    common::git(dir, &["config", "user.email", "test@example.com"]);
    common::git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), b"fn f() -> i32 {\n    1\n}\n").unwrap();
    common::git(dir, &["add", "-A"]);
    common::git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

// multi_thread + ≥2 worker threads: `common::boot()` spawns the daemon's
// serve task onto this SAME runtime, and `assert_cmd`'s `.assert()` below
// synchronously blocks a worker thread waiting on the `kb-code` child
// process. On the default single-threaded `#[tokio::test]` flavor that
// blocks the daemon's own task from ever running, so the child's HTTP GET
// times out — mirrors kb-cli's `list_and_export_round_trip_against_daemon`
// (crates/kb-cli/tests/comments.rs), which hit the same trap first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identity_json_flag_prints_daemon_identity() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = common::boot(repo_tmp.path(), "kb").await;
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
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = common::boot(repo_tmp.path(), "kb").await;
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
