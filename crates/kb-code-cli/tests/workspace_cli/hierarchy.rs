//! `kb-code callees` / `callers` / `implementors` — V3.1-H1 CLI smoke tests
//! against a real daemon. Mirrors `tests/agentview.rs` boot conventions.

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

    std::fs::write(
        dir.join("hier.rs"),
        r#"
trait Draw { fn draw(&self); }
struct Circle;
impl Draw for Circle {
    fn draw(&self) { helper(); }
}
fn helper() {}
fn run(c: &Circle) {
    c.draw();
    helper();
}
"#,
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "hierarchy fixture"]);
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
        kb_daemon: kb_code_server::config::KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"), task)
}

async fn wait_for_indexed(url: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        if let Ok(resp) = client.get(format!("{url}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    // file_count trips before the per-file DERIVED rows land
                    // (symbols/occurrences/call_sites) — under a loaded
                    // parallel run the gap is real (2026-08-02: hierarchy
                    // smoke saw symbols:[] after this wait). symbol_count
                    // comes from the same extraction visit.
                    let symbols = entry["symbol_count"].as_u64().unwrap_or(0);
                    if count >= expected_files && symbols > 0 {
                        return;
                    }
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected repo {repo:?} file_count >= {expected_files}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn find_fn_pos(url: &str, repo: &str, path: &str, name: &str) -> (u32, u32) {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{url}/api/symbols"))
        .query(&[("repo", repo), ("path", path)])
        .send()
        .await
        .expect("symbols");
    let body: serde_json::Value = resp.json().await.expect("json");
    for s in body["symbols"].as_array().expect("symbols") {
        if s["name"].as_str() == Some(name) {
            return (
                s["line_start"].as_u64().unwrap() as u32,
                s["col_start"].as_u64().unwrap_or(0) as u32,
            );
        }
    }
    panic!("fn {name} not found in {path}: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callees_smoke_human_and_json() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;
    let (line, col) = find_fn_pos(&url, "fixture", "hier.rs", "run").await;
    let target = format!("hier.rs:{line}:{col}");

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["callees", &target, "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("helper"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "callees", &target, "--daemon", &url, "--repo", "fixture", "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("json");
    assert_eq!(body["schema"], "hierarchy/1");
    assert!(body["callees"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| { c["name"].as_str() == Some("helper") || c["name"].as_str() == Some("draw") }));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callers_smoke_json() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;
    let (line, col) = find_fn_pos(&url, "fixture", "hier.rs", "helper").await;
    let target = format!("hier.rs:{line}:{col}");

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "callers", &target, "--daemon", &url, "--repo", "fixture", "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("json");
    assert_eq!(body["schema"], "hierarchy/1");
    assert!(!body["callers"].as_array().unwrap().is_empty());
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn implementors_smoke_json() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;

    // V70-H1 — `wait_for_indexed`'s own doc comment already admits
    // `symbol_count` is a PROXY, not the real signal this test needs:
    // hierarchy's `call_sites`/`type_relations` rows are written LATER in
    // the SAME `index_file` visit (`ingest.rs`'s per-file order is
    // symbols -> occurrences -> todos -> imports -> hierarchy), so a poll
    // landing right after symbols land can still see empty `subtypes`.
    // `callees_smoke_human_and_json`/`callers_smoke_json` never hit this
    // because they only ever read the symbols endpoint `wait_for_indexed`
    // already waited on; `implementors` is the one query in this file that
    // needs the LATER hierarchy write. Retry the query itself (same shape
    // as `wait_for_indexed`'s own `/api/repos` retry) instead of asserting
    // on the first response — under a loaded host that window is real.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut body: serde_json::Value;
    loop {
        let out = Command::cargo_bin("kb-code")
            .unwrap()
            .args([
                "implementors",
                "Draw",
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
        body = serde_json::from_slice(&out).expect("json");
        let found = body["subtypes"]
            .as_array()
            .is_some_and(|subs| subs.iter().any(|s| s["name"].as_str() == Some("Circle")));
        if found || std::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(body["schema"], "hierarchy/1");
    let subs = body["subtypes"].as_array().unwrap();
    assert!(
        subs.iter().any(|s| s["name"].as_str() == Some("Circle")),
        "expected Circle implementor: {body}"
    );
    task.abort();
}
