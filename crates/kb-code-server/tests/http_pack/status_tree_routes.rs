//! V70-A3X — end-to-end HTTP tests for `GET /api/tree?worktree=1` and
//! `GET /api/status`, against a real daemon booted via
//! `serve_on_random_port_with_paths` over a real git fixture repo. Mirrors
//! `refs_diff_routes.rs`'s own conventions (own small `boot_with_repos`/
//! `git` helpers — this crate has no shared `tests/support` module yet).
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running
//! (the workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

async fn boot_with_repos(
    repos: &[(&str, &Path)],
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: repos
            .iter()
            .map(|(name, path)| RepoEntry {
                name: name.to_string(),
                path: std::fs::canonicalize(path).unwrap(),
            })
            .collect(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), b"// lib\n").unwrap();
    std::fs::write(dir.join("README.md"), b"# hi\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_default_is_byte_identical_odb_read_when_worktree_absent() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/tree"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body.get("worktree").is_none(), "body: {body}");
    let entries = body["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"src"), "got {names:?}");
    assert!(names.contains(&"README.md"), "got {names:?}");
    // The ODB shape carries `oid` — the worktree shape (below) never does.
    assert!(entries.iter().all(|e| e.get("oid").is_some()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_worktree_flag_reports_untracked_files_and_omits_ignored_and_deleted() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    // Untracked (must appear), gitignored (must NOT), tracked-then-deleted
    // (must NOT).
    std::fs::write(dir.join("src/untracked.rs"), b"// new\n").unwrap();
    std::fs::write(dir.join(".gitignore"), b"ignored.rs\n").unwrap();
    std::fs::write(dir.join("src/ignored.rs"), b"// ignored\n").unwrap();
    git(&dir, &["add", ".gitignore"]);
    git(&dir, &["commit", "-q", "-m", "c2"]);
    std::fs::write(dir.join("src/gone.rs"), b"// gone\n").unwrap();
    git(&dir, &["add", "src/gone.rs"]);
    git(&dir, &["commit", "-q", "-m", "c3"]);
    std::fs::remove_file(dir.join("src/gone.rs")).unwrap();

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/tree"))
        .query(&[("repo", "fixture"), ("path", "src"), ("worktree", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["worktree"], true, "body: {body}");
    let entries = body["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"lib.rs"), "got {names:?}");
    assert!(names.contains(&"untracked.rs"), "got {names:?}");
    assert!(!names.contains(&"ignored.rs"), "got {names:?}");
    assert!(!names.contains(&"gone.rs"), "got {names:?}");
    // The worktree shape carries no `oid` (see `WorktreeEntry`'s doc).
    assert!(entries.iter().all(|e| e.get("oid").is_none()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_modified_and_untracked_paths() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    std::fs::write(dir.join("README.md"), b"# changed\n").unwrap();
    std::fs::write(dir.join("new_file.txt"), b"new\n").unwrap();

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/status"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["dirty"], true, "body: {body}");
    let paths = body["paths"].as_array().unwrap();
    let readme = paths.iter().find(|p| p["path"] == "README.md").unwrap();
    assert_eq!(readme["worktree"], "M");
    let untracked = paths.iter().find(|p| p["path"] == "new_file.txt").unwrap();
    assert_eq!(untracked["index"], "?");
    assert_eq!(untracked["worktree"], "?");
    assert!(body["generation"].is_number());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_clean_for_a_freshly_committed_tree() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/status"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["dirty"], false, "body: {body}");
    assert_eq!(body["paths"], serde_json::json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_on_an_unknown_repo_404s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/status"))
        .query(&[("repo", "nope")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
