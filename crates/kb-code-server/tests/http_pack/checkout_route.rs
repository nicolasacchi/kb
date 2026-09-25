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
            url: Some("http://127.0.0.1:0".to_string()),
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

/// `for-each-ref` of `dir` — never lists `HEAD`, so byte-identical
/// before/after is exactly "only HEAD moved, no ref was written"
/// (BUILD-BRIEF U8's invariance test).
fn ref_tree(dir: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["for-each-ref", "--format=%(refname) %(objectname)"])
        .output()
        .expect("git runs");
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_of_a_store_only_review_tip_fetches_by_sha_and_writes_no_refs_kbc() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_two_branches();
    let dir = repo_tmp.path();
    let (daemon_tmp, base) = boot_with_repo("r", dir).await;

    // A commit that will exist ONLY inside a kb-owned review store — an
    // unrelated repo, fetched into a bare store under a review-ref name
    // that never touches `dir`'s own refs (ancestry doesn't matter:
    // `allowAnySHA1InWant` fetches by sha regardless). Built under a
    // SEPARATE tempdir, never under `dir` itself — `dir` IS the checked-out
    // repo's own working tree, and a stray subdirectory there would make
    // `git status` see it as untracked and trip the dirty-tree refusal.
    let scratch_tmp = tempfile::tempdir().unwrap();
    let scratch = scratch_tmp.path();
    let src = scratch.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    git(&src, &["config", "user.email", "test@example.com"]);
    git(&src, &["config", "user.name", "Test"]);
    std::fs::write(src.join("b.txt"), "review tip\n").unwrap();
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "review tip"]);
    let tip = git_out(&src, &["rev-parse", "HEAD"]);

    let store_dir = scratch.join("store.git");
    git(
        scratch,
        &["init", "-q", "--bare", store_dir.to_str().unwrap()],
    );
    std::fs::write(
        store_dir.join("config"),
        "[core]\n\trepositoryformatversion = 0\n\tbare = true\n\
         [uploadpack]\n\tallowAnySHA1InWant = true\n",
    )
    .unwrap();
    git(
        &store_dir,
        &[
            "fetch",
            "-q",
            src.to_str().unwrap(),
            "HEAD:refs/kbc/review/1/ps1",
        ],
    );

    // Register a `ready` review store for repo "r" directly on the
    // daemon's own sqlite volume — the seeding job that would normally
    // produce this row is a separate unit; this test only needs the DB
    // shape a `ready` store leaves behind (same precedent
    // `tests/review/local_review_routes.rs::verdict_zero_patchset_is_400`
    // uses for writing against a live daemon's own db).
    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    let repo_id = store
        .upsert_repo("r", std::fs::canonicalize(dir).unwrap().to_str().unwrap())
        .unwrap();
    let store_id = store
        .create_review_store(
            "22222222-2222-2222-2222-222222222222",
            "local:r-rs-u8-test",
            store_dir.to_str().unwrap(),
            None,
            None,
            1,
        )
        .unwrap();
    assert!(store
        .set_review_store_state(store_id, "ready", None)
        .unwrap());
    store.add_repo_to_store(repo_id, store_id).unwrap();
    drop(store);

    let before = ref_tree(dir);
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/checkout"))
        .json(&serde_json::json!({ "repo": "r", "ref": tip }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["detached"], true);
    assert_eq!(git_out(dir, &["rev-parse", "HEAD"]), tip);
    assert_eq!(
        ref_tree(dir),
        before,
        "checkout via the store bridge must write no ref besides HEAD"
    );
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
