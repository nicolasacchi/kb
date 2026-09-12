//! V3.1-H2 — `kb-code impact PATH:LINE:COL` + `kb-code lenses` smoke tests.

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
        dir.join("app.rs"),
        r#"
pub fn seed() {}
pub fn caller() { seed(); }
"#,
    )
    .unwrap();
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
        }],
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
        .expect("serve");
    (tmp, format!("http://{addr}"), task)
}

async fn wait_for_indexed(url: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if let Ok(resp) = client.get(format!("{url}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    let symbols = entry["symbol_count"].as_u64().unwrap_or(0);
                    if count >= expected_files && symbols > 0 {
                        return;
                    }
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected indexed symbols"
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
    panic!("fn {name} not found: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impact_analysis_cli_happy_path() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;
    let (line, col) = find_fn_pos(&url, "fixture", "app.rs", "seed").await;
    let target = format!("app.rs:{line}:{col}");

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["impact", &target, "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("impact analysis"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "impact", &target, "--daemon", &url, "--repo", "fixture", "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["schema"], "impact/1");
    assert_eq!(v["symbol"]["name"], "seed");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lenses_cli_happy_path() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["lenses", "app.rs", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("lenses"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "lenses", "app.rs", "--daemon", &url, "--repo", "fixture", "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["schema"], "lenses/1");
    assert!(!v["declarations"].as_array().unwrap().is_empty());
    task.abort();
}
