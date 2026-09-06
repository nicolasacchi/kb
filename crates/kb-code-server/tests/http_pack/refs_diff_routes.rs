//! W4.1/W4.2 — end-to-end HTTP tests for `GET /api/refs` and
//! `GET /api/diff`, against a real daemon booted via
//! `serve_on_random_port_with_paths` over a real git fixture repo. Mirrors
//! `tests/search_routes.rs`'s own conventions (`boot_with_repos`, the
//! `SERIAL` guard, a real `git` fixture built via `std::process::Command`)
//! rather than sharing them — this crate has no `tests/support` module yet
//! (see that file's own doc for why each e2e file duplicates this small
//! helper set). Unlike the search-lane routes, `refs`/`diff` need no
//! `wait_until_async` polling for a background index warm-up: both are
//! synchronous ODB/subprocess reads that never depend on the boot-time
//! HEAD-tree walk (`refs` is a pure gix ref-table read; `diff` shells out
//! to `git diff` directly) — the daemon is ready to answer them the moment
//! `serve_on_random_port_with_paths` returns.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running
//! (the workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, RepoEntry};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refs_lists_branches_and_tags_with_head_marked() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"one\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    git(&dir, &["branch", "feature/x"]);
    git(&dir, &["tag", "-a", "v1.0", "-m", "release"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/refs"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let refs = body["refs"].as_array().expect("refs array");
    let names: Vec<&str> = refs.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"main"), "got: {names:?}");
    assert!(names.contains(&"feature/x"), "got: {names:?}");
    assert!(names.contains(&"v1.0"), "got: {names:?}");

    let main_entry = refs.iter().find(|r| r["name"] == "main").unwrap();
    assert_eq!(main_entry["kind"], "branch");
    assert_eq!(main_entry["is_head"], true);
    let tag_entry = refs.iter().find(|r| r["name"] == "v1.0").unwrap();
    assert_eq!(tag_entry["kind"], "tag");
    assert_eq!(tag_entry["is_head"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refs_remotes_flag_adds_a_separate_remotes_array_and_is_byte_identical_when_absent() {
    let _guard = SERIAL.lock().await;
    // A bare local repo has no `origin` remote to point at — build a
    // second "remote" repo, clone it, so `refs/remotes/origin/*` actually
    // exists.
    let origin_tmp = tempfile::tempdir().unwrap();
    let origin_dir = std::fs::canonicalize(origin_tmp.path()).unwrap();
    init_repo(&origin_dir);
    std::fs::write(origin_dir.join("a.txt"), b"one\n").unwrap();
    git(&origin_dir, &["add", "-A"]);
    git(&origin_dir, &["commit", "-q", "-m", "c1"]);

    let clone_tmp = tempfile::tempdir().unwrap();
    let clone_dir = clone_tmp.path().join("clone");
    let out = Command::new("git")
        .args([
            "clone",
            "-q",
            origin_dir.to_str().unwrap(),
            clone_dir.to_str().unwrap(),
        ])
        .output()
        .expect("git clone runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = std::fs::canonicalize(&clone_dir).unwrap();

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    // Absent `remotes` — byte-identical to the pre-existing shape (no
    // `remotes` key at all).
    let plain: serde_json::Value = client
        .get(format!("{base}/api/refs"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(plain.get("remotes").is_none(), "body: {plain}");

    let with_remotes: serde_json::Value = client
        .get(format!("{base}/api/refs"))
        .query(&[("repo", "fixture"), ("remotes", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // `refs` itself is unaffected by the flag.
    assert_eq!(with_remotes["refs"], plain["refs"]);
    let remotes = with_remotes["remotes"].as_array().expect("remotes array");
    assert!(
        remotes
            .iter()
            .any(|r| r["name"] == "main" && r["remote"] == "origin"),
        "got: {remotes:?}"
    );
    // Local `refs` never gains a remote-tracking entry.
    let local_names: Vec<&str> = with_remotes["refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert!(
        !local_names.contains(&"origin/main"),
        "local refs must stay remote-free: {local_names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refs_on_an_unknown_repo_404s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"one\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/refs"))
        .query(&[("repo", "nope")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_between_two_refs_returns_the_hunk() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "line1\nline2\nline3\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    let sha1 = git_out(&dir, &["rev-parse", "HEAD"]);

    std::fs::write(dir.join("a.txt"), "line1\nCHANGED\nline3\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c2"]);
    let sha2 = git_out(&dir, &["rev-parse", "HEAD"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/diff"))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.txt"),
            ("from", sha1.as_str()),
            ("to", sha2.as_str()),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let diff = body["diff"].as_str().expect("diff text");
    assert!(diff.contains("-line2"), "got: {diff}");
    assert!(diff.contains("+CHANGED"), "got: {diff}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_with_no_to_compares_against_the_working_tree() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "line1\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    let sha1 = git_out(&dir, &["rev-parse", "HEAD"]);

    // Uncommitted working-tree edit — never staged/committed.
    std::fs::write(dir.join("a.txt"), "line1\nuncommitted\n").unwrap();

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/diff"))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.txt"),
            ("from", sha1.as_str()),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let diff = body["diff"].as_str().expect("diff text");
    assert!(diff.contains("+uncommitted"), "got: {diff}");
    assert_eq!(body["to"], serde_json::Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_on_an_unknown_repo_404s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"one\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/diff"))
        .query(&[("repo", "nope"), ("path", "a.txt"), ("from", "HEAD")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_rejects_a_path_traversal_attempt() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"one\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/diff"))
        .query(&[
            ("repo", "fixture"),
            ("path", "../etc/passwd"),
            ("from", "HEAD"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_on_an_unresolvable_ref_400s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"one\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/diff"))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.txt"),
            ("from", "deadbeefdeadbeefdead"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}
