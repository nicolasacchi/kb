//! `kb-code review` — V3.R1 CLI smoke tests against a real daemon.
//! Mirrors `tests/sets.rs` boot/fixture conventions.

use crate::common::git;
use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use predicates::str::contains;
use std::path::Path;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("lib.rs"), "fn a() { /* x */ }\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c2"]);
    tmp
}

async fn boot(
    repo_dir: &Path,
    repo_name: &str,
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"), task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_start_list_snapshot_viewed_flow() {
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_tmp, url, _task) = boot(dir, "fixture").await;

    // start
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "review",
            "start",
            "feature",
            "--repo",
            "fixture",
            "--base",
            "main",
            "--title",
            "my review",
            "--daemon",
            &url,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let created: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["latest_ps"], 1);

    // list
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["review", "list", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("my review"));

    // amend + snapshot
    std::fs::write(dir.join("lib.rs"), "fn a() { /* y */ }\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--amend", "-m", "c2b"]);
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["review", "snapshot", &id.to_string(), "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("ps2"));

    // files + viewed
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["review", "files", &id.to_string(), "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("lib.rs"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "review",
            "viewed",
            &id.to_string(),
            "lib.rs",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("viewed"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["review", "close", &id.to_string(), "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("closed"));
}
