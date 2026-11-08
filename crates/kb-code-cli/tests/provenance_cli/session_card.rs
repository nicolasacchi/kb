//! `kb-code session-card <SID>` — A5 CLI smoke tests against a REAL running
//! kb-code daemon. The daemon's `[kb_daemon]` is disabled, so the kb lanes
//! degrade (the human view must still print their captions) while the wire
//! body stays a complete `kbc-session-card/1`.

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
    std::fs::write(dir.join("a.txt"), b"x\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "fixture"]);
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
            projects: Vec::new(),
        }],
        // Never risk a real network call to an operator's own kb daemon —
        // the degraded lanes are the behaviour under test here.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_card_json_prints_the_wire_body() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["session-card", "sess-0001", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    assert_eq!(body["schema"], "kbc-session-card/1");
    assert_eq!(body["sid"], "sess-0001");
    for lane in ["commits", "reviews", "findings", "presence"] {
        assert!(body[lane].is_object(), "lane {lane} missing: {body}");
    }
    assert_eq!(body["commits"]["available"], false);
    assert_eq!(body["commits"]["caption"], "kb disabled");
    assert_eq!(body["reviews"]["available"], true);
    assert_eq!(body["findings"]["available"], true);
    assert_eq!(body["presence"]["caption"], "kb disabled");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_card_human_view_prints_lane_captions() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["session-card", "sess-0001", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("kb disabled"))
        .stdout(contains("commits:"))
        .stdout(contains("reviews:"))
        .stdout(contains("findings:"))
        .stdout(contains("presence:"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_card_rejects_a_malformed_sid_nonzero() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["session-card", "a/b", "--daemon", &url])
        .assert()
        .failure();
    task.abort();
}
