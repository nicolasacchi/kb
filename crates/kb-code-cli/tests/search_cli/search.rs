//! `kb-code search files|symbols|text` — W2.1 CLI smoke tests against a
//! REAL running kb-code daemon. Mirrors `tests/daemon.rs`'s own `boot()`/
//! `wait_for_json`/`fixture_repo` conventions and `worker_threads = 2`
//! rationale (see that file's doc comment).

use crate::common::{boot, git, wait_for_json};
use assert_cmd::Command;
use predicates::str::contains;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("gizmo_widget.rs"),
        b"fn known_symbol_name() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_files_finds_the_known_file() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    wait_for_json(
        &format!("{url}/api/search/files?repo=fixture&q=gizmo"),
        std::time::Duration::from_secs(15),
        |b| b["hits"].as_array().is_some_and(|a| !a.is_empty()),
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search", "files", "gizmo", "--daemon", &url, "--repo", "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("gizmo_widget.rs"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search", "files", "gizmo", "--daemon", &url, "--repo", "fixture", "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    assert!(body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["path"] == "gizmo_widget.rs"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_symbols_finds_the_known_function() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    wait_for_json(
        &format!("{url}/api/search/symbols?repo=fixture&q=known_symbol"),
        std::time::Duration::from_secs(15),
        |b| b["hits"].as_array().is_some_and(|a| !a.is_empty()),
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search",
            "symbols",
            "known_symbol",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("known_symbol_name"))
        .stdout(contains("gizmo_widget.rs"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_text_literal_mode_finds_the_known_line() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    wait_for_json(
        &format!("{url}/api/search/text?repo=fixture&q=known_symbol_name"),
        std::time::Duration::from_secs(15),
        |b| b["results"].as_array().is_some_and(|a| !a.is_empty()),
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search",
            "text",
            "known_symbol_name",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("gizmo_widget.rs"))
        .stdout(contains("fn known_symbol_name"));
    task.abort();
}

#[test]
fn search_symbols_unreachable_daemon_fails_cleanly() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search",
            "symbols",
            "anything",
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}
