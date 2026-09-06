//! Phase G-server ("kb-code v2 — The Operable Reader," the review-workflow
//! endpoints) — end-to-end HTTP tests for `GET /api/merge-check`,
//! `GET /api/repo-state`, `GET /api/range-diff`, and the GitHub read
//! overlay (`GET /api/prs`, `GET /api/prs/{number}/comments`, `POST
//! /api/prs/fetch`), against a real daemon booted via
//! `serve_on_random_port_with_paths`. Mirrors `tests/time_routes.rs`'s own
//! conventions (`boot`-style helper, a real `git` fixture via
//! `std::process::Command`) rather than sharing them — this crate has no
//! `tests/support` module yet (see `tests/refs_diff_routes.rs`'s own doc
//! for why each e2e file duplicates this small helper set).
//!
//! The GitHub-API-calling routes (`prs`/`prs/{n}/comments`) are tested
//! against a LOCAL mock GitHub server (a bare axum router bound to
//! loopback, `mock_github_server` below — duplicated from `join::
//! kb_client::test_support::mock_kb_server`'s shape rather than imported:
//! that helper is `pub(crate)`, invisible to this crate-external
//! integration-test binary), redirected to via `[github] api_base` — no
//! test in this file ever reaches the real network.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running
//! (the workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours
//! it.

use crate::common::{git, init_repo};
use axum::routing::get;
use axum::{Json, Router};
use kb_code_server::config::{GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use tokio::net::TcpListener;
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

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: "http://127.0.0.1:0".to_string(),
        token_file: None,
        public_url: None,
    }
}

async fn boot(cfg: KbCodeConfig) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        ..KbCodeConfig::default()
    };
    boot(cfg).await
}

/// Spin up a minimal axum server on an ephemeral loopback port — the
/// "mock GitHub API" every PR-lane test in this file uses instead of
/// depending on the real network. See the module doc.
async fn mock_github_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

// --- GET /api/merge-check --------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merge_check_reports_clean_for_a_fast_forwardable_pair() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["branch", "feature"]);
    git(&dir, &["checkout", "-q", "feature"]);
    std::fs::write(dir.join("b.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature change"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/merge-check"))
        .query(&[("repo", "fixture"), ("from", "main"), ("to", "feature")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "merge-check/1");
    assert_eq!(body["clean"], true);
    assert_eq!(body["conflicts"].as_array().unwrap().len(), 0);
    assert_eq!(body["ahead"], 1);
    assert_eq!(body["behind"], 0);
    assert!(body["resolved"]["merge_base"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merge_check_reports_conflicts_for_two_branches_editing_the_same_line() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["branch", "feature"]);
    git(&dir, &["checkout", "-q", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature edits a"]);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("a.txt"), "main\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "main edits a"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/merge-check"))
        .query(&[("repo", "fixture"), ("from", "main"), ("to", "feature")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["clean"], false);
    let conflicts: Vec<&str> = body["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(conflicts, vec!["a.txt"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merge_check_rejects_a_dash_prefixed_from_with_400() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/merge-check"))
        .query(&[
            ("repo", "fixture"),
            ("from", "--output=/tmp/pwned"),
            ("to", "main"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --- GET /api/repo-state ----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_state_reports_none_for_an_ordinary_clean_repo() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/repo-state"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "repo-state/1");
    assert_eq!(body["op"], "none");
    assert_eq!(body["detail"], serde_json::json!({}));
    assert_eq!(body["conflicted"].as_array().unwrap().len(), 0);
    assert_eq!(body["dirty"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repo_state_reports_merge_op_and_conflicted_paths_during_a_real_conflict() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["branch", "feature"]);
    git(&dir, &["checkout", "-q", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature edits a"]);
    git(&dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("a.txt"), "main\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "main edits a"]);

    // Start (and leave mid-flight) a real conflicting merge.
    let merge = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["merge", "feature"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the merge must actually conflict");

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/repo-state"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["op"], "merge");
    assert!(body["detail"]["head_sha"].is_string());
    let conflicted: Vec<&str> = body["conflicted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(conflicted, vec!["a.txt"]);
    assert_eq!(body["dirty"], true);

    // Cleanup: abort the merge so the fixture dir is left tidy.
    Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["merge", "--abort"])
        .status()
        .unwrap();
}

// --- GET /api/range-diff -----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_diff_reports_modified_and_added_across_a_real_rebase_amend() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("base.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    let base_sha = git_out(&dir, &["rev-parse", "HEAD"]);

    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "add a"]);
    git(&dir, &["tag", "topic-v1"]);

    git(&dir, &["checkout", "-q", "-b", "topic-v2", "topic-v1"]);
    git(&dir, &["commit", "--amend", "-q", "-m", "add a (amended)"]);
    std::fs::write(dir.join("c.txt"), "new\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "add c"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let old_range = format!("{base_sha}..topic-v1");
    let new_range = format!("{base_sha}..topic-v2");
    let body: serde_json::Value = client
        .get(format!("{base}/api/range-diff"))
        .query(&[
            ("repo", "fixture"),
            ("old", &old_range),
            ("new", &new_range),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "range-diff/1");
    assert_eq!(body["truncated"], false);
    let pairs = body["pairs"].as_array().unwrap();
    assert_eq!(pairs.len(), 2);
    assert_eq!(pairs[0]["disposition"], "modified");
    assert_eq!(pairs[0]["old_subject"], "add a");
    assert!(pairs[0].get("new_subject").is_none());
    assert_eq!(pairs[1]["disposition"], "added");
    assert_eq!(pairs[1]["new_subject"], "add c");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_diff_rejects_a_dash_prefixed_old_with_400() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/range-diff"))
        .query(&[
            ("repo", "fixture"),
            ("old", "--output=/tmp/pwned"),
            ("new", "main"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --- GET /api/prs, GET /api/prs/{n}/comments (mocked GitHub API) ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prs_route_lists_open_prs_from_a_mocked_github_api() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );

    let gh_router = Router::new().route(
        "/repos/acme/widget/pulls",
        get(|| async {
            Json(serde_json::json!([
                {
                    "number": 7,
                    "title": "Add feature",
                    "user": {"login": "octocat"},
                    "head": {"ref": "feature-branch"},
                    "base": {"ref": "main"},
                    "updated_at": "2024-01-01T00:00:00Z",
                    "draft": false
                }
            ]))
        }),
    );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/prs"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "prs/1");
    assert!(body.get("unavailable_reason").is_none());
    let prs = body["prs"].as_array().unwrap();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0]["number"], 7);
    assert_eq!(prs[0]["title"], "Add feature");
    assert_eq!(prs[0]["author"], "octocat");
    assert_eq!(prs[0]["head_ref"], "feature-branch");
    assert_eq!(prs[0]["base_ref"], "main");
    assert_eq!(prs[0]["draft"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prs_route_degrades_to_unavailable_reason_on_a_403_never_500s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );

    let gh_router = Router::new().route(
        "/repos/acme/widget/pulls",
        get(|| async { axum::http::StatusCode::FORBIDDEN }),
    );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/prs"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "a github-side failure must never 500");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["prs"].as_array().unwrap().len(), 0);
    assert!(body["unavailable_reason"]
        .as_str()
        .unwrap()
        .contains("rate-limited"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prs_route_rejects_a_non_github_origin_with_400() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    git(
        &dir,
        &["remote", "add", "origin", "https://gitlab.com/a/b.git"],
    );

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/prs"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_comments_route_merges_review_and_issue_comments() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );

    let gh_router = Router::new()
        .route(
            "/repos/acme/widget/pulls/7/comments",
            get(|| async {
                Json(serde_json::json!([
                    {
                        "user": {"login": "reviewer"},
                        "body": "nit: rename this",
                        "path": "src/lib.rs",
                        "line": 42,
                        "created_at": "2024-01-01T00:00:00Z"
                    }
                ]))
            }),
        )
        .route(
            "/repos/acme/widget/issues/7/comments",
            get(|| async {
                Json(serde_json::json!([
                    {
                        "user": {"login": "author"},
                        "body": "thanks!",
                        "created_at": "2024-01-02T00:00:00Z"
                    }
                ]))
            }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/prs/7/comments"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "pr-comments/1");
    let comments = body["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["path"], "src/lib.rs");
    assert_eq!(comments[0]["line"], 42);
    assert!(
        comments[1].get("path").is_none(),
        "an issue comment has no path"
    );
    assert_eq!(comments[1]["author"], "author");
}

// --- POST /api/prs/fetch (a local bare repo acting as its own origin) -----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prs_fetch_creates_the_refs_kbc_namespace_from_a_local_bare_origin() {
    let _guard = SERIAL.lock().await;

    // A bare repo standing in for "origin" — a real PR ref is simulated by
    // pushing straight to `refs/pull/<n>/head` (GitHub itself never lets a
    // client push there directly; this reproduces the same shape locally
    // with no network dependency).
    let bare_tmp = tempfile::tempdir().unwrap();
    let bare_dir = std::fs::canonicalize(bare_tmp.path())
        .unwrap()
        .join("origin.git");
    std::fs::create_dir_all(&bare_dir).unwrap();
    git(&bare_dir, &["init", "-q", "--bare", "-b", "main"]);

    let work_tmp = tempfile::tempdir().unwrap();
    let work_dir = std::fs::canonicalize(work_tmp.path()).unwrap();
    init_repo(&work_dir);
    std::fs::write(work_dir.join("a.txt"), "hello\n").unwrap();
    git(&work_dir, &["add", "-A"]);
    git(&work_dir, &["commit", "-q", "-m", "pr commit"]);
    let pr_sha = git_out(&work_dir, &["rev-parse", "HEAD"]);
    git(
        &work_dir,
        &[
            "push",
            "-q",
            bare_dir.to_str().unwrap(),
            "HEAD:refs/pull/42/head",
        ],
    );

    let repo_tmp = tempfile::tempdir().unwrap();
    let repo_dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&repo_dir);
    std::fs::write(repo_dir.join("base.txt"), "base\n").unwrap();
    git(&repo_dir, &["add", "-A"]);
    git(&repo_dir, &["commit", "-q", "-m", "base"]);
    git(
        &repo_dir,
        &["remote", "add", "origin", bare_dir.to_str().unwrap()],
    );

    let (_tmp, base) = boot_with_repo("fixture", &repo_dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/prs/fetch"))
        .json(&serde_json::json!({ "repo": "fixture", "number": 42 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["repo"], "fixture");
    assert_eq!(body["number"], 42);
    assert_eq!(body["ref"], "refs/kbc/pr/42");
    assert_eq!(body["sha"], pr_sha);

    assert_eq!(
        git_out(&repo_dir, &["rev-parse", "--verify", "refs/kbc/pr/42"]),
        pr_sha
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prs_fetch_reports_a_clean_error_for_an_unknown_pr_number() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    // No `origin` remote configured at all — `git fetch origin` fails
    // cleanly (never a panic).

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/prs/fetch"))
        .json(&serde_json::json!({ "repo": "fixture", "number": 999 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
}

// --- GET /api/prs/{n}, /checks, /reviews (PRR-R2, mocked GitHub API) ------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_route_returns_full_detail_for_a_merged_pr() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );

    let gh_router = Router::new().route(
        "/repos/acme/widget/pulls/7",
        get(|| async {
            Json(serde_json::json!({
                "number": 7,
                "title": "Add feature",
                "user": {"login": "octocat"},
                "head": {"ref": "feature-branch", "sha": "deadbeef"},
                "base": {"ref": "main"},
                "updated_at": "2024-01-01T00:00:00Z",
                "draft": false,
                "state": "closed",
                "merged": true,
                "labels": [{"name": "bug"}],
                "mergeable_state": "clean"
            }))
        }),
    );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/prs/7"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "pr-detail/1");
    assert!(body.get("unavailable_reason").is_none());
    assert_eq!(body["pr"]["number"], 7);
    assert_eq!(body["pr"]["state"], "closed");
    assert_eq!(body["pr"]["merged"], true);
    assert_eq!(body["pr"]["merge_state_status"], "clean");
    assert_eq!(body["pr"]["labels"][0], "bug");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_route_degrades_to_unavailable_reason_never_500s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );

    let gh_router = Router::new().route(
        "/repos/acme/widget/pulls/7",
        get(|| async { axum::http::StatusCode::NOT_FOUND }),
    );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/prs/7"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "a github-side failure must never 500");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["pr"].is_null());
    assert!(body["unavailable_reason"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_checks_route_reports_normalized_checks_for_the_prs_head_sha() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );

    let gh_router = Router::new()
        .route(
            "/repos/acme/widget/pulls/7",
            get(|| async {
                Json(serde_json::json!({
                    "number": 7,
                    "title": "Add feature",
                    "user": {"login": "octocat"},
                    "head": {"ref": "feature-branch", "sha": "deadbeef"},
                    "base": {"ref": "main"},
                    "updated_at": "2024-01-01T00:00:00Z",
                    "draft": false,
                    "state": "open",
                    "merged": false
                }))
            }),
        )
        .route(
            "/repos/acme/widget/commits/deadbeef/check-runs",
            get(|| async {
                Json(serde_json::json!({
                    "check_runs": [
                        {"name": "build", "status": "completed", "conclusion": "success"}
                    ]
                }))
            }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/prs/7/checks"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "pr-checks/1");
    let checks = body["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["name"], "build");
    assert_eq!(checks[0]["status"], "pass");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_reviews_route_reports_per_reviewer_state_and_decision() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    git(
        &dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );

    let gh_router = Router::new()
        .route(
            "/repos/acme/widget/pulls/7/reviews",
            get(|| async {
                Json(serde_json::json!([
                    {"user": {"login": "alice"}, "state": "APPROVED", "submitted_at": "2024-01-01T00:00:00Z"}
                ]))
            }),
        )
        .route(
            "/repos/acme/widget/pulls/7/requested_reviewers",
            get(|| async { Json(serde_json::json!({"users": [{"login": "bob"}]})) }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/prs/7/reviews"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "pr-reviews/1");
    assert_eq!(body["reviewers"][0]["reviewer"], "alice");
    assert_eq!(body["reviewers"][0]["state"], "APPROVED");
    assert_eq!(body["requested_reviewers"][0], "bob");
    assert_eq!(body["review_decision"], "APPROVED");
}

// --- POST /api/reviews/pr (PRR-R2) ------------------------------------------
//
// Fixture note: `github_repo()` needs `origin` CONFIGURED as a
// `https://github.com/...` URL (so it resolves an owner/repo pair), while
// `fetch_pr_ref`'s actual `git fetch origin ...` needs `origin` to be
// REACHABLE. `git remote get-url origin` (the OLD `origin_url`
// implementation) applies `url.*.insteadOf` rewrites, which would make it
// report the rewritten (local) target instead of the configured github.com
// URL — `github.rs`'s own `origin_url` was switched to `git config --get
// remote.origin.url` (which does NOT apply the rewrite) specifically so
// this fixture shape works: `origin` is configured as a github.com URL,
// `insteadOf` transparently redirects the ACTUAL fetch to a local bare
// repo. See `github::tests::github_repo_resolves_the_configured_url_even_
// under_an_insteadof_rewrite` for the isolated unit-level proof.
// NOTE: returns BOTH tempdirs (`repo_tmp` AND `bare_tmp`) — the bare
// "origin" is fetched from lazily (only when the caller later hits `POST
// /api/reviews/pr`), so it must stay alive for the WHOLE test, not just
// this function's body. Dropping `bare_tmp` at the end of this function
// (the first version of this fixture's bug) deletes `origin.git` before
// the daemon ever fetches from it — a `TempDir`'s `Drop` removes its
// directory unconditionally, regardless of what still references its path
// by string.
fn fixture_pr_repo(
    pr_number: u32,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    String,
) {
    let bare_tmp = tempfile::tempdir().unwrap();
    let bare_dir = std::fs::canonicalize(bare_tmp.path())
        .unwrap()
        .join("origin.git");
    std::fs::create_dir_all(&bare_dir).unwrap();
    git(&bare_dir, &["init", "-q", "--bare", "-b", "main"]);

    let repo_tmp = tempfile::tempdir().unwrap();
    let repo_dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&repo_dir);
    std::fs::write(repo_dir.join("base.txt"), "base\n").unwrap();
    git(&repo_dir, &["add", "-A"]);
    git(&repo_dir, &["commit", "-q", "-m", "base"]);
    git(
        &repo_dir,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );
    git(
        &repo_dir,
        &[
            "config",
            &format!("url.{}.insteadOf", bare_dir.to_str().unwrap()),
            "https://github.com/acme/widget.git",
        ],
    );

    // The PR branch shares history with `main` (so `merge-base` succeeds
    // once its ref is fetched back) — pushed to the bare "origin" as the
    // simulated PR ref (real GitHub never lets a client push directly to
    // `refs/pull/<n>/head`; this reproduces the same shape locally,
    // mirroring `github.rs`'s own `fetch_pr_ref_creates_the_refs_kbc_
    // namespace...` fixture).
    git(&repo_dir, &["checkout", "-q", "-b", "pr-branch"]);
    std::fs::write(repo_dir.join("feature.txt"), "feature\n").unwrap();
    git(&repo_dir, &["add", "-A"]);
    git(&repo_dir, &["commit", "-q", "-m", "pr commit"]);
    let pr_sha = git_out(&repo_dir, &["rev-parse", "HEAD"]);
    git(
        &repo_dir,
        &[
            "push",
            "-q",
            bare_dir.to_str().unwrap(),
            &format!("HEAD:refs/pull/{pr_number}/head"),
        ],
    );
    git(&repo_dir, &["checkout", "-q", "main"]);
    git(&repo_dir, &["branch", "-D", "pr-branch"]);

    (repo_tmp, bare_tmp, repo_dir, pr_sha)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_review_pr_binds_captures_ps1_and_enriches_metadata() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) = fixture_pr_repo(42);

    let gh_router = Router::new()
        .route(
            "/repos/acme/widget/pulls/42",
            get({
                let pr_sha = pr_sha.clone();
                move || {
                    let pr_sha = pr_sha.clone();
                    async move {
                        Json(serde_json::json!({
                            "number": 42,
                            "title": "Add feature",
                            "user": {"login": "octocat"},
                            "head": {"ref": "pr-branch", "sha": pr_sha},
                            "base": {"ref": "main"},
                            "updated_at": "2024-01-01T00:00:00Z",
                            "draft": false,
                            "state": "open",
                            "merged": false,
                            "labels": [{"name": "bug"}],
                            "mergeable_state": "clean"
                        }))
                    }
                }
            }),
        )
        .route(
            &format!("/repos/acme/widget/commits/{pr_sha}/check-runs"),
            get(|| async {
                Json(serde_json::json!({
                    "check_runs": [
                        {"name": "build", "status": "completed", "conclusion": "success"}
                    ]
                }))
            }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 42 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["pr_number"], 42);
    assert_eq!(body["pr_repo_slug"], "acme/widget");
    assert_eq!(body["pr_head_sha"], pr_sha);
    assert_eq!(body["latest_ps"], 1);
    // Built via the ad-hoc `json!` macro (matching `create_review`/
    // `get_review`'s own convention elsewhere in `reviews.rs` — e.g.
    // `verdict_block`'s "always the key, null when absent" shape): an
    // absent reason is present-but-`null`, never an omitted key.
    assert!(
        body["pr_meta_unavailable_reason"].is_null(),
        "{}",
        body["pr_meta_unavailable_reason"]
    );
    assert_eq!(body["pr_meta"]["title"], "Add feature");
    assert_eq!(body["pr_meta"]["checks"][0]["status"], "pass");
    let id = body["id"].as_i64().unwrap();

    // The fetched ref really landed in the repo's own ref-db.
    assert_eq!(
        git_out(&dir, &["rev-parse", "--verify", "refs/kbc/pr/42"]),
        pr_sha
    );

    // GET /api/reviews/{id} reflects the bound review.
    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(show["head_ref"], "refs/kbc/pr/42");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_review_pr_degrades_metadata_on_a_github_side_403_but_still_creates_the_review() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, _pr_sha) = fixture_pr_repo(43);

    let gh_router = Router::new().route(
        "/repos/acme/widget/pulls/43",
        get(|| async { axum::http::StatusCode::FORBIDDEN }),
    );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 43 }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "a github-side metadata failure must never sink review creation"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["pr_meta"].is_null());
    assert!(body["pr_meta_unavailable_reason"]
        .as_str()
        .unwrap()
        .contains("rate-limited"));
    assert_eq!(body["latest_ps"], 1, "ps1 is still captured");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_review_pr_rejects_a_duplicate_binding_with_409_naming_the_existing_id() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, _pr_sha) = fixture_pr_repo(44);

    // No GitHub mock needed for the metadata call — the origin isn't
    // github-shaped is fine too, but here we reuse the github-shaped
    // fixture and simply let metadata enrichment 404/degrade (no route
    // registered on this bare mock router), since the 409 pre-check fires
    // before any of that.
    let gh_router = Router::new();
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 44 }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 201, "{}", first.text().await.unwrap());
    let first_body: serde_json::Value = first.json().await.unwrap();
    let existing_id = first_body["id"].as_i64().unwrap();

    let second = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 44 }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 409);
    let second_body: serde_json::Value = second.json().await.unwrap();
    assert_eq!(second_body["existing_review_id"], existing_id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_review_pr_reports_400_on_a_failed_fetch_and_writes_no_row() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    // No `origin` remote at all — the fetch fails cleanly, and (unlike
    // `/prs/fetch`'s own 500) this route maps that to 400 (design doc §2
    // row 1: "the git fetch ... is load-bearing (400 on failure)").

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 999 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // No review was created.
    let list: serde_json::Value = client
        .get(format!("{base}/api/reviews"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list["reviews"].as_array().unwrap().is_empty());
}

// --- GET/PUT /api/reviews/{id}/report (PRR-R2) ------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_report_get_defaults_to_null_put_replaces_wholesale_and_emits_generated_at() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({"repo": "fixture", "head_ref": "feature", "base_ref": "main"}))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), 201);
    let id = create.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // GET before any report exists.
    let before: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/report"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(before["report"].is_null());

    // PUT — generated_at is server-stamped, overwriting the client-supplied
    // value (`1`) rather than merging it in.
    let put = client
        .put(format!("{base}/api/reviews/{id}/report"))
        .json(&serde_json::json!({
            "schema": "kbc-review-report/1",
            "summary": "Looks good, one blocker",
            "risk_score": 4,
            "generated_at": 1,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        put.status(),
        200,
        "{}",
        put.text().await.unwrap_or_default()
    );
    let put_body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/report"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(put_body["summary"], "Looks good, one blocker");
    assert_eq!(put_body["risk_score"], 4);
    assert_ne!(
        put_body["generated_at"], 1,
        "generated_at is server-stamped, not client-supplied"
    );
    assert!(put_body["generated_at"].as_i64().unwrap() > 1);

    // A SECOND put wholesale-replaces (never merges) — the first put's
    // `risk_score`/`schema` fields do not survive if omitted.
    let second = client
        .put(format!("{base}/api/reviews/{id}/report"))
        .json(&serde_json::json!({"summary": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 200);
    let after: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/report"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["summary"], "x");
    assert!(
        after.get("risk_score").is_none(),
        "wholesale replace must drop fields the second PUT omitted"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_report_put_folds_a_nested_generator_verdict_into_flat_fields() {
    // V70-A3X — the operator's report GENERATOR writes a nested
    // `verdict: {headline, body}`; the SPA's Report tab reads the FLAT
    // `verdict_headline`/`verdict_body` fields. Before this fix, a
    // generator-authored report was stored opaquely and silently degraded
    // to "Unset" on read.
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({"repo": "fixture", "head_ref": "feature", "base_ref": "main"}))
        .send()
        .await
        .unwrap();
    let id = create.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let put = client
        .put(format!("{base}/api/reviews/{id}/report"))
        .json(&serde_json::json!({
            "verdict": {"headline": "Ships it", "body": "Clean diff, no blockers."},
            "summary": "Looks good",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        put.status(),
        200,
        "{}",
        put.text().await.unwrap_or_default()
    );
    let put_body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/report"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(put_body.get("verdict").is_none(), "body: {put_body}");
    assert_eq!(put_body["verdict_headline"], "Ships it");
    assert_eq!(put_body["verdict_body"], "Clean diff, no blockers.");
    assert_eq!(put_body["summary"], "Looks good");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_report_put_400s_problem_json_on_an_unknown_field() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({"repo": "fixture", "head_ref": "feature", "base_ref": "main"}))
        .send()
        .await
        .unwrap();
    let id = create.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = client
        .put(format!("{base}/api/reviews/{id}/report"))
        .json(&serde_json::json!({"summary": "ok", "totally_made_up_field": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/problem+json"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "urn:kb:errors:report-shape");
    assert_eq!(body["status"], 400);
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("totally_made_up_field"),
        "detail must name the offending key: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_report_put_400s_on_a_review_with_zero_patchsets() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (daemon_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    // `POST /api/reviews` always captures ps1 — a true zero-patchset review
    // is only reachable by inserting the row directly, mirroring
    // `local_review_routes.rs`'s own `verdict_zero_patchset_is_400`
    // precedent for the exact same store-level gap.
    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    let id = store
        .create_review("fixture", None, "main", "feature", None, 1)
        .unwrap();
    drop(store);

    let resp = client
        .put(format!("{base}/api/reviews/{id}/report"))
        .json(&serde_json::json!({"summary": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "{}", resp.text().await.unwrap());
}

// --- GET /api/reviews/{id}/artifact (PRR-R2) --------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_artifact_reports_hint_null_when_unset() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({"repo": "fixture", "head_ref": "feature", "base_ref": "main"}))
        .send()
        .await
        .unwrap();
    let id = create.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/artifact"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["hint"].is_null());
    assert_eq!(body["verified"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_artifact_reports_unavailable_when_kb_daemon_is_disabled() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({"repo": "fixture", "head_ref": "feature", "base_ref": "main"}))
        .send()
        .await
        .unwrap();
    let id = create.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // Set the hint via PATCH (loopback, design doc §2 row 6) before
    // verifying it — the PATCH response itself echoes the hint pair.
    let patch = client
        .patch(format!("{base}/api/reviews/{id}"))
        .json(&serde_json::json!({"artifact_hint_kb": "platform", "artifact_hint_id": "abc123"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        patch.status(),
        200,
        "{}",
        patch.text().await.unwrap_or_default()
    );
    let patch_body: serde_json::Value = client
        .patch(format!("{base}/api/reviews/{id}"))
        .json(&serde_json::json!({"artifact_hint_kb": "platform", "artifact_hint_id": "abc123"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(patch_body["artifact_hint_kb"], "platform");
    assert_eq!(patch_body["artifact_hint_id"], "abc123");

    let body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/artifact"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // `disabled_kb_daemon()` (this file's fixture default) means
    // `KbClient::doc_meta` fails with `KbClientError::Disabled` — an
    // honest degrade, never a 500.
    assert_eq!(body["hint"]["kb"], "platform");
    assert_eq!(body["hint"]["id"], "abc123");
    assert_eq!(body["verified"], false);
    assert!(body["unavailable_reason"].is_string());
}

// --- PRR-R4: additive PR-binding/report fields on GET /reviews[/{id}] ------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn additive_pr_binding_and_report_fields_are_null_on_a_plain_unbound_review() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({"repo": "fixture", "head_ref": "feature", "base_ref": "main"}))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), 201);
    let id = create.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Pre-existing fields still present (additive-only contract).
    assert_eq!(show["id"], id);
    assert_eq!(show["repo"], "fixture");
    // New fields are honestly null/false, never fabricated.
    assert!(show["pr_number"].is_null());
    assert!(show["pr_repo_slug"].is_null());
    assert!(show["pr_head_sha"].is_null());
    assert!(show["pr_meta"].is_null());
    assert!(show["pr_meta_fetched_at"].is_null());
    assert!(show["artifact_hint_kb"].is_null());
    assert!(show["artifact_hint_id"].is_null());
    assert_eq!(show["has_report"], false);
    assert!(show["report_risk_score"].is_null());

    let list: serde_json::Value = client
        .get(format!("{base}/api/reviews"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = &list["reviews"].as_array().unwrap()[0];
    assert_eq!(row["id"], id);
    assert!(row["pr_number"].is_null());
    assert_eq!(row["has_report"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn additive_pr_binding_and_report_fields_reflect_a_bound_review_with_a_report() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) = fixture_pr_repo(50);

    let gh_router = Router::new()
        .route(
            "/repos/acme/widget/pulls/50",
            get({
                let pr_sha = pr_sha.clone();
                move || {
                    let pr_sha = pr_sha.clone();
                    async move {
                        Json(serde_json::json!({
                            "number": 50,
                            "title": "Add feature",
                            "user": {"login": "octocat"},
                            "head": {"ref": "pr-branch", "sha": pr_sha},
                            "base": {"ref": "main"},
                            "updated_at": "2024-01-01T00:00:00Z",
                            "draft": false,
                            "state": "open",
                            "merged": false,
                            "labels": [],
                            "mergeable_state": "clean"
                        }))
                    }
                }
            }),
        )
        .route(
            &format!("/repos/acme/widget/commits/{pr_sha}/check-runs"),
            get(|| async { Json(serde_json::json!({ "check_runs": [] })) }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 50 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let put = client
        .put(format!("{base}/api/reviews/{id}/report"))
        .json(&serde_json::json!({"summary": "ok", "risk_score": 3}))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), 200);

    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(show["pr_number"], 50);
    assert_eq!(show["pr_repo_slug"], "acme/widget");
    assert_eq!(show["pr_head_sha"], pr_sha);
    assert_eq!(show["pr_meta"]["title"], "Add feature");
    assert_eq!(show["has_report"], true);
    assert_eq!(show["report_risk_score"], 3);

    let list: serde_json::Value = client
        .get(format!("{base}/api/reviews"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = list["reviews"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap();
    assert_eq!(row["pr_number"], 50);
    assert_eq!(row["has_report"], true);
    assert_eq!(row["report_risk_score"], 3);
}

// --- PRR-R4: GET /api/reviews/{id}/pr-status --------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_status_400s_when_the_review_is_not_pr_bound() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "feature"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({"repo": "fixture", "head_ref": "feature", "base_ref": "main"}))
        .send()
        .await
        .unwrap();
    let id = create.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = client
        .get(format!("{base}/api/reviews/{id}/pr-status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

fn pr_status_gh_router(pr_number: u32, pr_repo: &str, head_sha: String) -> Router {
    Router::new().route(
        &format!("/repos/{pr_repo}/pulls/{pr_number}"),
        get(move || {
            let head_sha = head_sha.clone();
            async move {
                Json(serde_json::json!({
                    "number": pr_number,
                    "title": "t",
                    "user": {"login": "octocat"},
                    "head": {"ref": "pr-branch", "sha": head_sha},
                    "base": {"ref": "main"},
                    "updated_at": "2024-01-01T00:00:00Z",
                    "draft": false,
                    "state": "open",
                    "merged": false,
                    "labels": [],
                    "mergeable_state": "clean"
                }))
            }
        }),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_status_reports_local_match_and_live_success_with_zero_commits_behind() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) = fixture_pr_repo(60);
    let gh_router = pr_status_gh_router(60, "acme/widget", pr_sha.clone());
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 60 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let status: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/pr-status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["pr_number"], 60);
    assert_eq!(status["review_snapshot_head_sha"], pr_sha);
    assert_eq!(status["latest_local_ps_tip_sha"], pr_sha);
    assert_eq!(status["local_matches_pr"], true);
    assert_eq!(status["stale"], false);
    assert!(status["unavailable_reason"].is_null());
    assert_eq!(status["pr_head_sha"], pr_sha);
    assert_eq!(status["commits_behind"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_status_reports_local_mismatch_after_a_further_local_commit_without_a_pr_refresh() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) = fixture_pr_repo(61);
    let gh_router = pr_status_gh_router(61, "acme/widget", pr_sha.clone());
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 61 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // Move `refs/kbc/pr/61` forward locally (simulating a later `pr fetch`
    // that landed new commits) WITHOUT re-binding — `pr_head_sha` on the
    // review row stays at the ORIGINAL snapshot.
    git(&dir, &["checkout", "-q", "--detach", "refs/kbc/pr/61"]);
    std::fs::write(dir.join("more.txt"), "more\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "more work"]);
    let new_sha = git_out(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["update-ref", "refs/kbc/pr/61", &new_sha]);
    git(&dir, &["checkout", "-q", "main"]);

    let snap = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(snap.status(), 200, "{}", snap.text().await.unwrap());

    let status: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/pr-status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["review_snapshot_head_sha"], pr_sha);
    assert_eq!(status["latest_local_ps_tip_sha"], new_sha);
    assert_eq!(status["local_matches_pr"], false);
    assert_eq!(status["stale"], true);
    // The live half still degrades honestly even though the mock reports
    // the ORIGINAL sha (never the moved one) — commits_behind is 0 because
    // the live head hasn't moved relative to itself, only local advanced
    // past it.
    assert!(status["unavailable_reason"].is_null());
    assert_eq!(status["pr_head_sha"], pr_sha);
    assert_eq!(
        status["commits_behind"], 0,
        "the live head (pr_sha) is an ancestor of the local tip, so 0 commits sit \
         between the local tip and the (unmoved) live head"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pr_status_degrades_the_live_half_when_github_is_unavailable() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) = fixture_pr_repo(62);
    // A GitHub mock that 403s the pull-detail call — RateLimited.
    let gh_router = Router::new().route(
        "/repos/acme/widget/pulls/62",
        get(|| async { axum::http::StatusCode::FORBIDDEN }),
    );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    };
    let (_tmp, base) = boot(cfg).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": 62 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let status: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/pr-status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // LOCAL half still answers, even though the live GitHub call fails.
    assert_eq!(status["review_snapshot_head_sha"], pr_sha);
    assert_eq!(status["latest_local_ps_tip_sha"], pr_sha);
    assert_eq!(status["local_matches_pr"], true);
    assert_eq!(status["stale"], false);
    // LIVE half degrades honestly.
    assert!(status["pr_head_sha"].is_null());
    assert!(status["commits_behind"].is_null());
    assert!(status["unavailable_reason"]
        .as_str()
        .unwrap()
        .contains("rate-limited"));
}
