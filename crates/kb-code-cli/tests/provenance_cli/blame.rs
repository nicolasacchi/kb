//! `kb-code blame`/`kb-code timeline` — W3.1 CLI smoke tests against a REAL
//! running kb-code daemon. Mirrors `tests/search.rs`'s own `boot()`/
//! `wait_for_json`/fixture-repo conventions.

use crate::common::{boot, git};
use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;

fn commit_as(dir: &Path, name: &str, email: &str, msg: &str) {
    git(dir, &["config", "user.name", name]);
    git(dir, &["config", "user.email", email]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);

    std::fs::write(dir.join("known_file.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    commit_as(
        dir,
        "Alice",
        "alice@example.com",
        "c1: alice adds known_file",
    );

    std::fs::write(dir.join("known_file.rs"), "fn a() {}\nfn b_edited() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    commit_as(dir, "Bob", "bob@example.com", "c2: bob edits fn b");

    tmp
}

async fn wait_for_indexed(url: &str, repo: &str, path: &str) {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let ok = client
            .get(format!("{url}/api/file"))
            .query(&[("repo", repo), ("path", path)])
            .send()
            .await
            .is_ok_and(|r| r.status().is_success());
        if ok || std::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_reports_both_authors() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", "known_file.rs").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "blame",
            "known_file.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("Alice"))
        .stdout(contains("Bob"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "blame",
            "known_file.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    assert_eq!(body["dirty"], false);
    let regions = body["regions"].as_array().unwrap();
    assert!(regions.iter().any(|r| r["author"] == "Alice"));
    assert!(regions.iter().any(|r| r["author"] == "Bob"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_lines_narrows_the_output() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", "known_file.rs").await;

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "blame",
            "known_file.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--lines",
            "2:2",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    let regions = body["regions"].as_array().unwrap();
    assert!(!regions.is_empty());
    assert!(regions.iter().all(|r| {
        let start = r["final_start"].as_u64().unwrap();
        let count = r["count"].as_u64().unwrap();
        start <= 2 && start + count > 2
    }));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeline_reports_history_newest_first() {
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    std::fs::write(
        dir.join("known_file.rs"),
        "fn a() {}\nfn b_edited_again() {}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    commit_as(
        dir,
        "Carol",
        "carol@example.com",
        "c3: carol edits fn b again",
    );

    let (_tmp, url, task) = boot(dir, "fixture").await;
    wait_for_indexed(&url, "fixture", "known_file.rs").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "timeline",
            "known_file.rs:2",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("c3: carol edits fn b again"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "timeline",
            "known_file.rs:2",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["subject"], "c3: carol edits fn b again");
    task.abort();
}

#[test]
fn blame_unreachable_daemon_fails_cleanly() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "blame",
            "anything.rs",
            "--repo",
            "fixture",
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}
