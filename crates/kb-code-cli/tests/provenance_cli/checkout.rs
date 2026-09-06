//! `kb-code checkout` — W4.7 CLI smoke tests against a REAL running
//! kb-code daemon. Mirrors `tests/blame.rs`'s own `boot()`/fixture-repo
//! conventions.

use crate::common::{boot, git};
use assert_cmd::Command;
use predicates::str::contains;
use std::process::Command as StdCommand;

fn fixture_two_branches() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["branch", "feature"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_switches_a_clean_tree_via_the_cli() {
    let repo_tmp = fixture_two_branches();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["checkout", "feature", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("switched to feature"));

    let head = StdCommand::new("git")
        .arg("-C")
        .arg(repo_tmp.path())
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "feature");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_prints_the_dirty_refusal_nicely() {
    let repo_tmp = fixture_two_branches();
    std::fs::write(repo_tmp.path().join("a.txt"), "modified\n").unwrap();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["checkout", "feature", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .failure()
        .stdout(contains("refused"))
        .stdout(contains("a.txt"));

    let head = StdCommand::new("git")
        .arg("-C")
        .arg(repo_tmp.path())
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&head.stdout).trim(),
        "main",
        "a refused checkout must not move HEAD"
    );
    task.abort();
}

#[test]
fn checkout_unreachable_daemon_fails_cleanly() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "checkout",
            "main",
            "--repo",
            "fixture",
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}
