//! W3.1 — end-to-end HTTP tests for the BLAME service
//! (`GET /api/blame`, `GET /api/blame/timeline`), against a real daemon
//! booted via `serve_on_random_port_with_paths` over a real git fixture
//! repo. Mirrors `tests/search_routes.rs`'s own conventions (`boot_with_repos`,
//! `wait_until_async`, the `SERIAL` guard).
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn commit_as(dir: &Path, name: &str, email: &str, msg: &str) {
    git(dir, &["config", "user.name", name]);
    git(dir, &["config", "user.email", email]);
    git(dir, &["commit", "-q", "-m", msg]);
}

async fn wait_until_async<F, Fut>(timeout: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if f().await {
            return true;
        }
        if Instant::now() >= deadline {
            return f().await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

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

/// Two commits, two authors, mirroring `blame::mod::tests::two_commit_repo`.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_repo(dir);
    std::fs::write(dir.join("f.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    commit_as(
        dir,
        "Alice",
        "alice@example.com",
        "c1: alice adds three fns",
    );

    std::fs::write(dir.join("f.rs"), "fn a() {}\nfn b_edited() {}\nfn c() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    commit_as(dir, "Bob", "bob@example.com", "c2: bob edits fn b");

    tmp
}

/// Waits for the daemon's initial HEAD-tree walk to have indexed `path` in
/// `repo` — every blame test needs this so `is_dirty`'s store lookup sees a
/// real `files` row (matching a real daemon's live-mirror behaviour) rather
/// than racing the boot-time walk.
async fn wait_for_indexed(base: &str, repo: &str, path: &str) {
    let client = reqwest::Client::new();
    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.to_string();
        let repo = repo.to_string();
        let path = path.to_string();
        async move {
            client
                .get(format!("{base}/api/file"))
                .query(&[("repo", repo.as_str()), ("path", path.as_str())])
                .send()
                .await
                .ok()
                .is_some_and(|r| r.status().is_success())
        }
    })
    .await;
    assert!(
        ok,
        "expected the initial index walk to make {path} readable"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_clean_file_reports_every_author_and_is_not_dirty() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", dir)]).await;
    wait_for_indexed(&base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/blame"))
        .query(&[("repo", "fixture"), ("path", "f.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["dirty"], false);
    let regions = body["regions"].as_array().unwrap();
    assert!(!regions.is_empty());
    let authors: Vec<&str> = regions
        .iter()
        .map(|r| r["author"].as_str().unwrap())
        .collect();
    assert!(authors.contains(&"Alice"));
    assert!(authors.contains(&"Bob"));

    // A second identical request must report a cache hit.
    let second: serde_json::Value = client
        .get(format!("{base}/api/blame"))
        .query(&[("repo", "fixture"), ("path", "f.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["cached"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_dirty_working_tree_file_is_reported_as_dirty() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", dir)]).await;
    wait_for_indexed(&base, "fixture", "f.rs").await;

    // Edit on disk without committing.
    std::fs::write(
        dir.join("f.rs"),
        "fn a() {}\nfn b_edited() {}\nfn c() {}\nfn d_uncommitted() {}\n",
    )
    .unwrap();

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/blame"))
        .query(&[("repo", "fixture"), ("path", "f.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["dirty"], true);
    assert_eq!(body["cached"], false);
    let regions = body["regions"].as_array().unwrap();
    let uncommitted_sha = "0".repeat(40);
    assert!(regions
        .iter()
        .any(|r| r["final_start"] == 4 && r["sha"] == uncommitted_sha.as_str()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_line_range_narrows_the_response() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", dir)]).await;
    wait_for_indexed(&base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/blame"))
        .query(&[
            ("repo", "fixture"),
            ("path", "f.rs"),
            ("start", "2"),
            ("end", "2"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let regions = body["regions"].as_array().unwrap();
    assert!(!regions.is_empty());
    assert!(regions.iter().all(|r| {
        let start = r["final_start"].as_u64().unwrap();
        let count = r["count"].as_u64().unwrap();
        start <= 2 && start + count > 2
    }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_unknown_path_404s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", dir)]).await;
    wait_for_indexed(&base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let status = client
        .get(format!("{base}/api/blame"))
        .query(&[("repo", "fixture"), ("path", "does-not-exist.rs")])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_unknown_repo_404s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", dir)]).await;
    wait_for_indexed(&base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let status = client
        .get(format!("{base}/api/blame"))
        .query(&[("repo", "no-such-repo"), ("path", "f.rs")])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn blame_timeline_is_bounded_and_newest_first() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    // A third commit touching line 2 again, so there are 3 history entries.
    std::fs::write(
        dir.join("f.rs"),
        "fn a() {}\nfn b_edited_again() {}\nfn c() {}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    commit_as(
        dir,
        "Carol",
        "carol@example.com",
        "c3: carol edits fn b again",
    );

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", dir)]).await;
    wait_for_indexed(&base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/blame/timeline"))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("line", "2")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["subject"], "c3: carol edits fn b again");
    assert_eq!(entries[2]["subject"], "c1: alice adds three fns");

    let limited: serde_json::Value = client
        .get(format!("{base}/api/blame/timeline"))
        .query(&[
            ("repo", "fixture"),
            ("path", "f.rs"),
            ("line", "2"),
            ("max", "1"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(limited["entries"].as_array().unwrap().len(), 1);
}
