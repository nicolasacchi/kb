//! `kb-code watch <lane>...` (V71-X1) — end-to-end smoke against a REAL
//! daemon, proving the CLI WIRING (lane validation, `--json` requirement,
//! multi-lane concurrent fan-out, the `"lane"` tag) rather than re-proving
//! `annotate watch`/`inbox --watch`'s own seed/diff/format logic (already
//! covered by `v4.rs`'s `annotate_watch_backlog_once_surfaces_and_exits`
//! and `inbox.rs`'s unit tests). Mirrors that file's `--backlog --once
//! --timeout` shape so a single call surfaces the current open set and
//! exits promptly instead of hanging on a live loop.

use crate::common::{boot, git};
use assert_cmd::Command;
use predicates::str::contains;
use std::time::Duration;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

fn kb() -> Command {
    Command::cargo_bin("kb-code").unwrap()
}

fn create_flag(url: &str, repo: &str, target: &str, body: &str) {
    kb().args([
        "annotate",
        target,
        "-m",
        body,
        "--intent",
        "flag-for-agent",
        "--daemon",
        url,
        "--repo",
        repo,
        "--json",
    ])
    .assert()
    .success();
}

/// A `flag-for-agent` annotation is visible to BOTH lanes at once: the
/// `annotate` lane's `GET /api/annotations/open` (every intent) and the
/// `inbox` lane's `GET /api/inbox` annotations sub-lane (which surfaces
/// exactly `question`/`flag-for-agent`, see `unified_inbox.rs`) — so
/// `--backlog` gives both lanes something to surface immediately and
/// `--once` lets the WHOLE process (which waits for every lane) exit
/// promptly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_lanes_surface_the_same_open_flag_tagged_by_lane() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    create_flag(&url, "fixture", "lib.rs:1", "watch-unified-marker");

    kb().args([
        "watch",
        "annotate",
        "inbox",
        "--repo",
        "fixture",
        "--daemon",
        &url,
        "--backlog",
        "--once",
        "--timeout",
        "5",
        "--json",
    ])
    .timeout(Duration::from_secs(20))
    .assert()
    .success()
    .stdout(contains("\"lane\":\"annotate\""))
    .stdout(contains("\"lane\":\"inbox\""))
    .stdout(contains("watch-unified-marker"));

    task.abort();
}

// The next two are pure CLI-validation checks (rejected before any
// network call), so they run against an address nothing listens on rather
// than paying for a daemon boot.
#[test]
fn unknown_lane_is_rejected_with_a_usage_error() {
    kb().args([
        "watch",
        "not-a-lane",
        "--repo",
        "fixture",
        "--daemon",
        "http://127.0.0.1:1",
        "--once",
        "--json",
    ])
    .assert()
    .failure()
    .stderr(contains("unknown lane"));
}

#[test]
fn non_json_output_is_refused_before_touching_the_daemon() {
    kb().args(["watch", "inbox", "--daemon", "http://127.0.0.1:1", "--once"])
        .assert()
        .failure()
        .stderr(contains("--json"));
}

/// `--since` in the past surfaces an already-open flag on the first read,
/// same as `--backlog` would — the "catch me up since I last watched"
/// window this unit adds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn since_in_the_past_surfaces_an_already_open_flag() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    create_flag(&url, "fixture", "lib.rs:1", "since-marker");

    kb().args([
        "watch",
        "annotate",
        "--repo",
        "fixture",
        "--daemon",
        &url,
        "--since",
        "0",
        "--once",
        "--timeout",
        "5",
        "--json",
    ])
    .timeout(Duration::from_secs(20))
    .assert()
    .success()
    .stdout(contains("since-marker"));

    task.abort();
}
