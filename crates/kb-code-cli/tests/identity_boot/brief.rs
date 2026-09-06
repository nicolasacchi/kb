//! `kb-code brief` (V71-X1 v0) — end-to-end against a REAL daemon,
//! proving the repo-resolution ladder (mirrors `doctor_cmd`'s own) and
//! the `/api/inbox` repo-scoped filter/re-count this verb adds client
//! side (no new route — see `Cmd::Brief`'s doc).

use crate::common::{boot, git};
use assert_cmd::Command;
use predicates::str::contains;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_repo_reports_its_own_open_flag_only() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    kb().args([
        "annotate",
        "lib.rs:1",
        "-m",
        "brief-marker",
        "--intent",
        "flag-for-agent",
        "--daemon",
        &url,
        "--repo",
        "fixture",
        "--json",
    ])
    .assert()
    .success();

    let out = kb()
        .args(["brief", "--repo", "fixture", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(body["schema"], "kbc-brief/1");
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["repo"], "fixture");
    assert_eq!(body["data"]["annotations"]["count"], 1);
    assert_eq!(body["data"]["annotations"]["flags_for_agent"], 1);
    let items = body["data"]["annotations"]["items"]
        .as_array()
        .expect("items array");
    assert!(items.iter().any(|a| a["excerpt"] == "brief-marker"));

    kb().args(["brief", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("kb-code brief — fixture"))
        .stdout(contains("brief-marker"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cwd_resolves_the_repo_when_repo_flag_is_omitted() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    let out = kb()
        .current_dir(repo_tmp.path())
        .args(["brief", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(body["data"]["repo"], "fixture");

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_repo_is_rejected_naming_the_configured_ones() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    kb().args(["brief", "--repo", "does-not-exist", "--daemon", &url])
        .assert()
        .failure()
        .stderr(contains("not a configured repo"))
        .stderr(contains("fixture"));

    task.abort();
}
