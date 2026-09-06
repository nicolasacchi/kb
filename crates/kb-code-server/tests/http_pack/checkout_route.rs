//! W4.7 — end-to-end HTTP tests for `POST /api/checkout`, against a real
//! daemon booted via `serve_on_random_port_with_paths` over a real git
//! fixture repo. Mirrors `tests/join_route.rs`'s own conventions
//! (`boot_with_repo`-style helper, `SERIAL` guard).
//!
//! `/api/checkout` is mounted on the LOOPBACK-ONLY sub-router (`router.rs`,
//! W4.7), same as `search/transcripts`/`session-diff` — every request here
//! is a real loopback TCP connection (no `X-Forwarded-For` spoofing), so
//! it passes that gate exactly like every other test in this crate that
//! already exercises a loopback-only route (`tests/transcripts.rs`).
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::git;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

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

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_refuses_and_lists_dirty_paths() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_two_branches();
    let dir = repo_tmp.path();
    std::fs::write(dir.join("a.txt"), "modified\n").unwrap();
    std::fs::write(dir.join("untracked.txt"), "new\n").unwrap();
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/checkout"))
        .json(&serde_json::json!({ "repo": "r", "ref": "feature" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    let mut dirty: Vec<String> = body["dirty_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    dirty.sort();
    assert_eq!(
        dirty,
        vec!["a.txt".to_string(), "untracked.txt".to_string()]
    );

    // HEAD must not have moved.
    assert_eq!(git_out(dir, &["symbolic-ref", "--short", "HEAD"]), "main");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_switches_a_clean_tree_to_a_local_branch() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_two_branches();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/checkout"))
        .json(&serde_json::json!({ "repo": "r", "ref": "feature" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["repo"], "r");
    assert_eq!(body["ref"], "feature");
    assert_eq!(body["detached"], false);
    assert_eq!(
        git_out(dir, &["symbolic-ref", "--short", "HEAD"]),
        "feature"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_detaches_for_a_raw_sha() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_two_branches();
    let dir = repo_tmp.path();
    let sha = git_out(dir, &["rev-parse", "HEAD"]);
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/checkout"))
        .json(&serde_json::json!({ "repo": "r", "ref": sha }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["detached"], true);

    let sym = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["symbolic-ref", "-q", "--short", "HEAD"])
        .output()
        .unwrap();
    assert!(!sym.status.success(), "HEAD must be detached");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_reports_a_clean_error_for_an_unknown_ref() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_two_branches();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/checkout"))
        .json(&serde_json::json!({ "repo": "r", "ref": "does-not-exist" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(git_out(dir, &["symbolic-ref", "--short", "HEAD"]), "main");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_reports_404_for_an_unknown_repo() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_two_branches();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/checkout"))
        .json(&serde_json::json!({ "repo": "does-not-exist", "ref": "main" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}
