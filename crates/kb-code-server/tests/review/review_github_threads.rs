//! PRR-R7 (design-addendum-2.md §A — GitHub thread import) — end-to-end
//! HTTP tests for `GET /api/reviews/{id}/github-threads`, against a real
//! daemon booted via `serve_on_random_port_with_paths`. Same conventions as
//! `review_routes.rs`'s own GitHub-overlay tests: a local mock GitHub
//! server redirected to via `[github] api_base`, `insteadOf`-rewritten
//! origin (see `review_routes.rs`'s `fixture_pr_repo` doc for why
//! `origin_url` reads `git config --get` rather than `git remote get-url`),
//! and each e2e file duplicates its own small helper set (no shared
//! `tests/support` module in this crate yet).
//!
//! Filter: `cargo test -p kb-code-server --test review github_threads`

use crate::common::{git, init_repo};
use axum::routing::get;
use axum::{Json, Router};
use kb_code_server::config::{GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
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

async fn mock_github_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

/// A repo whose `origin` is `insteadOf`-rewritten to a local bare "GitHub"
/// (see the module doc), with a PR branch adding `file_name` (content =
/// `file_lines` joined by `\n`, trailing newline) atop a `base.txt` base
/// commit, pushed to the bare origin as the simulated `refs/pull/<n>/head`.
/// Returns `(repo_tmp, bare_tmp, repo_dir, pr_sha)` — BOTH tempdirs must
/// stay alive for the whole test (the bare "origin" is fetched from lazily
/// by `POST /api/reviews/pr`), same discipline `review_routes.rs`'s own
/// `fixture_pr_repo` documents.
fn fixture_pr_repo_with_content(
    pr_number: u32,
    file_name: &str,
    file_lines: &[&str],
) -> (tempfile::TempDir, tempfile::TempDir, PathBuf, String) {
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

    git(&repo_dir, &["checkout", "-q", "-b", "pr-branch"]);
    let content = format!("{}\n", file_lines.join("\n"));
    std::fs::write(repo_dir.join(file_name), content).unwrap();
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

/// The `pulls/{n}` + `commits/{sha}/check-runs` routes `POST /api/reviews/pr`
/// needs to bind + enrich metadata — same minimal shape
/// `review_routes.rs`'s `fixture_pr_repo` test router uses.
fn gh_bind_router(pr_number: u32, pr_sha: &str) -> Router {
    let sha = pr_sha.to_string();
    Router::new()
        .route(
            &format!("/repos/acme/widget/pulls/{pr_number}"),
            get({
                let sha = sha.clone();
                move || {
                    let sha = sha.clone();
                    async move {
                        Json(serde_json::json!({
                            "number": pr_number,
                            "title": "Add feature",
                            "user": {"login": "octocat"},
                            "head": {"ref": "pr-branch", "sha": sha},
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
            &format!("/repos/acme/widget/commits/{sha}/check-runs"),
            get(|| async { Json(serde_json::json!({ "check_runs": [] })) }),
        )
}

/// `POST /api/reviews/pr` against `base`, asserting a clean bind, and
/// returning the new review id.
async fn bind_pr(client: &reqwest::Client, base: &str, repo: &str, pr_number: u32) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": repo, "pr_number": pr_number }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

// --- GET /api/reviews/{id}/github-threads -----------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_threads_maps_the_diff_hunks_last_line_onto_the_latest_patchset() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) =
        fixture_pr_repo_with_content(70, "feature.txt", &["line one", "line two", "line three"]);

    let gh_router = gh_bind_router(70, &pr_sha)
        .route(
            "/repos/acme/widget/pulls/70/comments",
            get(|| async {
                Json(serde_json::json!([
                    {
                        "id": 1,
                        "user": {"login": "reviewer"},
                        "body": "nit: rename this",
                        "path": "feature.txt",
                        "line": 2,
                        "diff_hunk": "@@ -0,0 +1,2 @@\n+line one\n+line two",
                        "created_at": "2024-01-01T00:00:00Z",
                        "html_url": "https://github.com/acme/widget/pull/70#discussion_r1"
                    }
                ]))
            }),
        )
        .route(
            "/repos/acme/widget/issues/70/comments",
            get(|| async { Json(serde_json::json!([])) }),
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
    let id = bind_pr(&client, &base, "fixture", 70).await;

    let body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/github-threads"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "kbc-github-threads/1");
    assert_eq!(body["pr_number"], 70);
    assert!(body["unavailable_reason"].is_null());
    let threads = body["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1);
    let t = &threads[0];
    assert_eq!(t["path"], "feature.txt");
    assert_eq!(t["author"], "reviewer");
    assert_eq!(
        t["html_url"],
        "https://github.com/acme/widget/pull/70#discussion_r1"
    );
    assert!(t.get("general").is_none());
    assert!(t.get("orphaned").is_none());
    assert_eq!(t["resolved"]["line"], 2);
    assert_eq!(t["resolved"]["confidence"], "exact");
    assert!(t["replies"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_threads_reports_an_honest_orphan_when_the_hunk_matches_nothing() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) =
        fixture_pr_repo_with_content(71, "feature.txt", &["line one", "line two", "line three"]);

    let gh_router = gh_bind_router(71, &pr_sha).route(
        "/repos/acme/widget/pulls/71/comments",
        get(|| async {
            Json(serde_json::json!([
                {
                    "id": 2,
                    "user": {"login": "reviewer"},
                    "body": "what is this referring to?",
                    "path": "feature.txt",
                    "line": 99,
                    "diff_hunk": "@@ -0,0 +1,1 @@\n+this text does not exist anywhere in the file",
                    "created_at": "2024-01-01T00:00:00Z"
                }
            ]))
        }),
    ).route(
        "/repos/acme/widget/issues/71/comments",
        get(|| async { Json(serde_json::json!([])) }),
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
    let id = bind_pr(&client, &base, "fixture", 71).await;

    let body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/github-threads"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let threads = body["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1);
    let t = &threads[0];
    assert_eq!(t["orphaned"], true, "a wrong line must never be guessed");
    assert!(t.get("resolved").is_none());
    assert!(t.get("general").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_threads_reports_general_true_for_an_issue_style_comment() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) =
        fixture_pr_repo_with_content(72, "feature.txt", &["line one"]);

    let gh_router = gh_bind_router(72, &pr_sha)
        .route(
            "/repos/acme/widget/pulls/72/comments",
            get(|| async { Json(serde_json::json!([])) }),
        )
        .route(
            "/repos/acme/widget/issues/72/comments",
            get(|| async {
                Json(serde_json::json!([
                    {
                        "id": 3,
                        "user": {"login": "author"},
                        "body": "thanks for the review!",
                        "created_at": "2024-01-02T00:00:00Z",
                        "html_url": "https://github.com/acme/widget/pull/72#issuecomment-1"
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
    let id = bind_pr(&client, &base, "fixture", 72).await;

    let body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/github-threads"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let threads = body["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1);
    let t = &threads[0];
    assert_eq!(t["general"], true);
    assert!(t["path"].is_null(), "an issue comment has no path");
    assert!(t.get("orphaned").is_none());
    assert!(t.get("resolved").is_none());
    assert_eq!(t["author"], "author");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_threads_nests_replies_under_the_root_via_in_reply_to_id() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) =
        fixture_pr_repo_with_content(73, "feature.txt", &["line one", "line two", "line three"]);

    let gh_router = gh_bind_router(73, &pr_sha)
        .route(
            "/repos/acme/widget/pulls/73/comments",
            get(|| async {
                Json(serde_json::json!([
                    {
                        "id": 100,
                        "user": {"login": "reviewer"},
                        "body": "nit: rename this",
                        "path": "feature.txt",
                        "line": 2,
                        "diff_hunk": "@@ -0,0 +1,2 @@\n+line one\n+line two",
                        "created_at": "2024-01-01T00:00:00Z"
                    },
                    {
                        "id": 101,
                        "user": {"login": "author"},
                        "body": "thanks for pointing that out",
                        "path": "feature.txt",
                        "line": 2,
                        "diff_hunk": "@@ -0,0 +1,2 @@\n+line one\n+line two",
                        "created_at": "2024-01-01T01:00:00Z",
                        "in_reply_to_id": 100
                    }
                ]))
            }),
        )
        .route(
            "/repos/acme/widget/issues/73/comments",
            get(|| async { Json(serde_json::json!([])) }),
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
    let id = bind_pr(&client, &base, "fixture", 73).await;

    let body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/github-threads"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let threads = body["threads"].as_array().unwrap();
    assert_eq!(
        threads.len(),
        1,
        "the reply must nest, not appear as its own root"
    );
    let t = &threads[0];
    assert_eq!(t["id"], 100);
    let replies = t["replies"].as_array().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["id"], 101);
    assert_eq!(replies[0]["body"], "thanks for pointing that out");
    assert_eq!(replies[0]["in_reply_to"], 100);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_threads_400s_when_the_review_is_not_pr_bound() {
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
        .get(format!("{base}/api/reviews/{id}/github-threads"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn github_threads_degrades_to_unavailable_reason_when_github_is_down_never_500s() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, pr_sha) =
        fixture_pr_repo_with_content(74, "feature.txt", &["line one"]);

    // The bind mocks succeed (so the review is genuinely PR-bound), but the
    // review-comments endpoint 403s — `list_pull_comments` fails on its
    // FIRST call, so the issues-comments route is never even reached.
    let gh_router = gh_bind_router(74, &pr_sha).route(
        "/repos/acme/widget/pulls/74/comments",
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
    let id = bind_pr(&client, &base, "fixture", 74).await;

    let resp = client
        .get(format!("{base}/api/reviews/{id}/github-threads"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "a github-side failure must never 500");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["threads"].as_array().unwrap().is_empty());
    assert!(body["unavailable_reason"]
        .as_str()
        .unwrap()
        // V76-R1c — a BARE 403 is `forbidden`; `rate-limited` needs
        // `X-RateLimit-Remaining: 0` (see `pr_status_degrades_the_live_half…`).
        .contains("forbidden"));
}
