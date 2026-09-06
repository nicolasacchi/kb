//! V4.P1 — branches / compare / merge-check / repo-state / review
//! comments / verdict CLI smoke against a real daemon.

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

fn kb() -> Command {
    Command::cargo_bin("kb-code").unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_compare_merge_check_repo_state_happy_paths() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    kb().args(["branches", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("(default)"))
        .stdout(contains("feature"));

    let out = kb()
        .args([
            "branches",
            "--repo",
            "fixture",
            "--sort",
            "suggested",
            "--daemon",
            &url,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(body["schema"], "branches/1");
    assert!(body["branches"].as_array().unwrap().len() >= 2);
    // suggested sort attaches a suggest block.
    assert!(body["branches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b.get("suggest").is_some()));

    kb().args([
        "compare", "main", "feature", "--repo", "fixture", "--daemon", &url,
    ])
    .assert()
    .success()
    .stdout(contains("compare"))
    .stdout(contains("lib.rs"));

    kb().args([
        "merge-check",
        "feature",
        "--repo",
        "fixture",
        "--daemon",
        &url,
    ])
    .assert()
    .success()
    .stdout(contains("clean"));

    kb().args(["repo-state", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("op: none"))
        .stdout(contains("clean"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_comments_and_verdict_happy_paths() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    let out = kb()
        .args([
            "review", "start", "feature", "--repo", "fixture", "--base", "main", "--title", "v4",
            "--daemon", &url, "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let created: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let id = created["id"].as_i64().unwrap();

    // Review-scoped comment via the HTTP surface (CLI annotate has no
    // --review yet). Then the new comments verb must list it.
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "fixture",
            "path": "lib.rs",
            "line": 1,
            "body": "looks off",
            "intent": "question",
            "review_id": id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let ann: serde_json::Value = resp.json().await.unwrap();
    let ann_id = ann["id"].as_str().unwrap();

    kb().args(["review", "comments", &id.to_string(), "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("lib.rs"))
        .stdout(contains("question"))
        .stdout(contains("looks off"));

    // Attach a suggestion so `suggest list --review` can see it.
    kb().args([
        "suggest",
        ann_id,
        "-m",
        "fn a() { /* ok */ }",
        "--daemon",
        &url,
    ])
    .assert()
    .success();
    kb().args([
        "suggest",
        "list",
        "--review",
        &id.to_string(),
        "--daemon",
        &url,
    ])
    .assert()
    .success()
    .stdout(contains(ann_id))
    .stdout(contains("pending"));

    let vout = kb()
        .args([
            "review",
            "verdict",
            &id.to_string(),
            "approve",
            "-m",
            "lgtm",
            "--daemon",
            &url,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let verdict: serde_json::Value = serde_json::from_slice(&vout).unwrap();
    assert_eq!(verdict["changed"], true);

    kb().args([
        "review",
        "verdict",
        &id.to_string(),
        "--clear",
        "--daemon",
        &url,
    ])
    .assert()
    .success()
    .stdout(contains("cleared"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_verdict_non_loopback_404_says_requires_loopback() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            use std::io::{Read, Write};
            let mut buf = [0u8; 512];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        }
    });

    kb().args([
        "review",
        "verdict",
        "1",
        "approve",
        "--daemon",
        &format!("http://{addr}"),
    ])
    .assert()
    .failure()
    .stderr(contains("requires loopback"));
}
