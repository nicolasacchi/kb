//! `kb-code why`/`story`/`provenance-report` — W3.3 + W3.4 CLI smoke tests
//! against a REAL running kb-code daemon. Mirrors `tests/blame.rs`'s own
//! `boot()`/`wait_for_indexed`/fixture-repo conventions.

use crate::common::git;
use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use predicates::str::contains;
use std::path::Path;

fn commit_with_message(dir: &Path, file: &str, contents: &str, message: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// A single trailer-stamped commit — enough for `why`/`story`'s CLI smoke
/// tests (the full arm/precedence matrix lives in
/// `kb_code_server::join::ladder`'s own unit tests; the daemon-level
/// confidence/session_id round-trip already lives in
/// `kb-code-server/tests/provenance_routes.rs`).
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);

    commit_with_message(
        dir,
        "known_file.rs",
        "fn a() {}\nfn b() {}\n",
        "the subject\n\nKb-Session: sess-cli\n",
    );
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
        // Never risk a real network call to an operator's own kb daemon —
        // the trailer arm resolves fully locally, which is all these
        // smoke tests need.
        kb_daemon: kb_code_server::config::KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
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
async fn why_line_reports_the_trailer_session() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", "known_file.rs").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "why",
            "known_file.rs:1",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("trailer"))
        .stdout(contains("sess-cli"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "why",
            "known_file.rs:1",
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
    assert_eq!(body["attribution"]["confidence"], "trailer");
    assert_eq!(body["attribution"]["session_id"], "sess-cli");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn why_file_grade_reports_the_dominant_session() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", "known_file.rs").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "why",
            "known_file.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("sess-cli"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "why",
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
    let sessions = body["sessions"].as_array().unwrap();
    assert!(!sessions.is_empty());
    assert_eq!(sessions[0]["session_id"], "sess-cli");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn story_reports_the_file_timeline() {
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    commit_with_message(
        dir,
        "known_file.rs",
        "fn a() {}\nfn b_edited() {}\n",
        "c2: plain edit",
    );

    let (_tmp, url, task) = boot(dir, "fixture").await;
    wait_for_indexed(&url, "fixture", "known_file.rs").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "story",
            "known_file.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("sess-cli"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "story",
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
    let entries = body["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    assert!(entries
        .iter()
        .any(|e| e["session_id"].as_str() == Some("sess-cli")));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provenance_report_prints_confidence_buckets() {
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    commit_with_message(
        dir,
        "known_file.rs",
        "fn a() {}\nfn b_edited() {}\n",
        "c2: plain edit",
    );

    let (_tmp, url, task) = boot(dir, "fixture").await;
    wait_for_indexed(&url, "fixture", "known_file.rs").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["provenance-report", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("by confidence"))
        .stdout(contains("trailer"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "provenance-report",
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
    assert_eq!(body["total_commits"], 2);
    let by_confidence = body["by_confidence"].as_array().unwrap();
    let trailer = by_confidence
        .iter()
        .find(|b| b["label"] == "trailer")
        .unwrap();
    assert_eq!(trailer["count"], 1);
    task.abort();
}

#[test]
fn why_unreachable_daemon_fails_cleanly() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "why",
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
