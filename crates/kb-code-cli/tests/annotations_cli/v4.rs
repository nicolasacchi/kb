//! V4.P1 — suggest / batch / watch CLI smoke against a real daemon.

use crate::common::{boot, git};
use assert_cmd::Command;
use predicates::str::contains;
use std::io::Write;
use std::time::Duration;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

fn kb() -> Command {
    Command::cargo_bin("kb-code").unwrap()
}

fn create_ann(url: &str, repo: &str, target: &str, body: &str) -> String {
    let out = kb()
        .args([
            "annotate", target, "-m", body, "--daemon", url, "--repo", repo, "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggest_put_list_apply_drop_round_trip() {
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_tmp, url, task) = boot(dir, "fixture").await;

    let id = create_ann(&url, "fixture", "lib.rs:2", "rewrite b");

    kb().args(["suggest", &id, "-m", "fn b() { /* x */ }", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("suggestion on"));

    // Apply changes the working tree.
    kb().args(["suggest", "apply", &id, "--daemon", &url, "--json"])
        .assert()
        .success()
        .stdout(contains("\"applied\": true"));
    let text = std::fs::read_to_string(dir.join("lib.rs")).unwrap();
    assert!(text.contains("fn b() { /* x */ }"), "{text}");

    // Second apply is already_applied → exit 0.
    kb().args(["suggest", "apply", &id, "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("already applied"));

    // Re-PUT then mutate the line so apply 409s.
    kb().args(["suggest", &id, "-m", "fn b() { /* y */ }", "--daemon", &url])
        .assert()
        .success();
    std::fs::write(
        dir.join("lib.rs"),
        "fn a() {}\nfn b() { DRIFT }\nfn c() {}\n",
    )
    .unwrap();
    kb().args(["suggest", "apply", &id, "--daemon", &url])
        .assert()
        .failure()
        .stderr(contains("conflict"))
        .stdout(contains("--- expected"))
        .stdout(contains("--- found"));

    kb().args(["suggest", "drop", &id, "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("dropped suggestion"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_batch_applies_ops_and_prints_created_ids() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    let mut ops = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        ops,
        r#"[
          {{"op":"add_comment","path":"lib.rs","line":1,"body":"first"}},
          {{"op":"add_comment","path":"lib.rs","line":3,"body":"third"}}
        ]"#
    )
    .unwrap();

    let out = kb()
        .args([
            "annotate",
            "batch",
            "--file",
            ops.path().to_str().unwrap(),
            "--repo",
            "fixture",
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
    assert_eq!(body["applied"], 2);
    assert_eq!(body["changed"], true);
    assert_eq!(body["created_ids"].as_array().unwrap().len(), 2);

    // Server 400s pass through readably.
    let mut bad = tempfile::NamedTempFile::new().unwrap();
    writeln!(bad, r#"[{{"op":"resolve","id":"ann_doesnotexist"}}]"#).unwrap();
    kb().args([
        "annotate",
        "batch",
        "--file",
        bad.path().to_str().unwrap(),
        "--repo",
        "fixture",
        "--daemon",
        &url,
    ])
    .assert()
    .failure()
    .stderr(contains("400"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_watch_backlog_once_surfaces_and_exits() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    create_ann(&url, "fixture", "lib.rs:1", "watch me");

    kb().args([
        "annotate",
        "watch",
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
    .timeout(Duration::from_secs(15))
    .assert()
    .success()
    .stdout(contains("\"kind\":\"annotation\""))
    .stdout(contains("watch me"));

    task.abort();
}

/// PRR-R6: a `main`+`feature` pair (mirrors
/// `provenance_cli/review.rs::fixture_repo`) so `review start` has a
/// real base..head diff to snapshot ps1 against.
fn fixture_repo_with_feature() -> tempfile::TempDir {
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

/// PRR-R6 (design doc §3.3, kb v0.39 T2 Phase 6) — `annotate watch
/// --review N` now also polls `GET /api/reviews/{id}/findings`
/// alongside `/comments`. Mirrors `annotate_watch_backlog_once_
/// surfaces_and_exits` above (same boot/backlog/--once/--json shape),
/// but review-scoped and against a manually-authored finding (`review
/// findings add`, addendum §E) rather than a plain annotation — cheap
/// to exercise end to end without a fake findings-import payload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_watch_review_scoped_surfaces_new_finding() {
    let repo_tmp = fixture_repo_with_feature();
    let dir = repo_tmp.path();
    let (_tmp, url, task) = boot(dir, "fixture").await;

    let out = kb()
        .args([
            "review",
            "start",
            "feature",
            "--repo",
            "fixture",
            "--base",
            "main",
            "--title",
            "watch test",
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
    let review_id = created["id"].as_i64().unwrap().to_string();

    kb().args([
        "review",
        "findings",
        "add",
        &review_id,
        "--severity",
        "concern",
        "--category",
        "Testing",
        "--path",
        "lib.rs",
        "--line",
        "1",
        "-m",
        "watch me finding",
        "--rationale",
        "because tests say so",
        "--daemon",
        &url,
    ])
    .assert()
    .success();

    kb().args([
        "annotate",
        "watch",
        "--review",
        &review_id,
        "--daemon",
        &url,
        "--backlog",
        "--once",
        "--timeout",
        "5",
        "--json",
    ])
    .timeout(Duration::from_secs(15))
    .assert()
    .success()
    .stdout(contains("\"kind\":\"finding\""))
    .stdout(contains("watch me finding"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggest_apply_non_loopback_404_says_requires_loopback() {
    // Bare-404 empty body — the loopback-gate shape — from a fake server.
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
        "suggest",
        "apply",
        "ann_x",
        "--daemon",
        &format!("http://{addr}"),
    ])
    .assert()
    .failure()
    .stderr(contains("requires loopback"));
}
