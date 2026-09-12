//! PRR-R8 ("The PR Room," kb v0.39 T2, design-addendum-2 §B) — end-to-end
//! HTTP tests for `POST /api/reviews/sweep`. Fixture/mock pattern
//! duplicated from `review_routes.rs`'s own PRR-R2 `POST /api/reviews/pr`
//! tests (`fixture_pr_repo`, `mock_github_server`, `boot`) — see that
//! file's module doc for why each e2e file in this crate keeps its own
//! small helper set rather than sharing one.

use crate::common::{git, init_repo};
use axum::routing::get;
use axum::{Json, Router};
use kb_code_server::config::{GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
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

async fn mock_github_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

fn cfg_for(dir: &Path, gh_addr: SocketAddr) -> KbCodeConfig {
    KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.to_path_buf(),
        }],
        kb_daemon: disabled_kb_daemon(),
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    }
}

/// One bare "origin" + working repo with `pr_numbers.len()` independent PR
/// branches, each pushed to `refs/pull/<n>/head` in the bare origin. See
/// `review_routes.rs`'s own `fixture_pr_repo` doc for the `insteadOf`
/// trick this borrows (github.com-shaped `origin` for `github_repo()`,
/// transparently redirected to the local bare repo for the actual fetch).
fn fixture_multi_pr_repo(
    pr_numbers: &[u32],
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    HashMap<u32, String>,
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

    let mut shas = HashMap::new();
    for &n in pr_numbers {
        let branch = format!("pr-{n}");
        git(&repo_dir, &["checkout", "-q", "-b", &branch, "main"]);
        std::fs::write(repo_dir.join(format!("feature-{n}.txt")), "feature\n").unwrap();
        git(&repo_dir, &["add", "-A"]);
        git(
            &repo_dir,
            &["commit", "-q", "-m", &format!("pr {n} commit")],
        );
        let sha = git_out(&repo_dir, &["rev-parse", "HEAD"]);
        git(
            &repo_dir,
            &[
                "push",
                "-q",
                bare_dir.to_str().unwrap(),
                &format!("HEAD:refs/pull/{n}/head"),
            ],
        );
        git(&repo_dir, &["checkout", "-q", "main"]);
        git(&repo_dir, &["branch", "-D", &branch]);
        shas.insert(n, sha);
    }
    (repo_tmp, bare_tmp, repo_dir, shas)
}

/// Simulate a GitHub-side push AFTER a review has already been bound:
/// commit on top of the ALREADY-locally-fetched `refs/kbc/pr/<n>` ref (so
/// the new head is locally resolvable, matching the module doc's "local
/// rev-list when the pr ref is present" condition), push it as the new
/// `refs/pull/<n>/head` in the bare origin, and land it locally under
/// `refs/kbc/pr/<n>` too (mimicking a prior `kb-code pr fetch`). Returns
/// the new head sha. The review's OWN patchset ref
/// (`refs/kbc/review/<id>/ps1`) is untouched — sweep never re-captures.
fn push_new_pr_head(repo_dir: &Path, bare_dir: &Path, pr_number: u32) -> String {
    let pr_ref = format!("refs/kbc/pr/{pr_number}");
    git(repo_dir, &["checkout", "-q", &pr_ref]);
    std::fs::write(
        repo_dir.join(format!("feature-{pr_number}-v2.txt")),
        "more\n",
    )
    .unwrap();
    git(repo_dir, &["add", "-A"]);
    git(repo_dir, &["commit", "-q", "-m", "pr update"]);
    let new_sha = git_out(repo_dir, &["rev-parse", "HEAD"]);
    git(
        repo_dir,
        &[
            "push",
            "-q",
            bare_dir.to_str().unwrap(),
            &format!("HEAD:refs/pull/{pr_number}/head"),
        ],
    );
    git(repo_dir, &["update-ref", &pr_ref, "HEAD"]);
    git(repo_dir, &["checkout", "-q", "main"]);
    new_sha
}

fn pull_json(number: u32, head_sha: &str, state: &str, merged: bool) -> serde_json::Value {
    serde_json::json!({
        "number": number,
        "title": "Add feature",
        "user": {"login": "octocat"},
        "head": {"ref": format!("pr-{number}"), "sha": head_sha},
        "base": {"ref": "main"},
        "updated_at": "2024-01-01T00:00:00Z",
        "draft": false,
        "state": state,
        "merged": merged,
        "labels": [],
        "mergeable_state": "clean"
    })
}

fn checks_json(n: usize) -> serde_json::Value {
    let mut runs =
        vec![serde_json::json!({"name": "build", "status": "completed", "conclusion": "success"})];
    if n > 1 {
        runs.push(
            serde_json::json!({"name": "lint", "status": "completed", "conclusion": "success"}),
        );
    }
    serde_json::json!({ "check_runs": runs })
}

async fn bind_pr(client: &reqwest::Client, base: &str, pr_number: u32) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "fixture", "pr_number": pr_number }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn sweep(
    client: &reqwest::Client,
    base: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/sweep"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    let json: serde_json::Value = if text.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(&text).unwrap()
    };
    (status, json)
}

fn row_for(body: &serde_json::Value, review_id: i64) -> &serde_json::Value {
    body["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["review_id"] == review_id)
        .unwrap_or_else(|| panic!("no row for review {review_id} in {body}"))
}

// --- SSE helpers (duplicated per this crate's own e2e-file convention) -----

async fn collect_sse_matching<S, B, E>(
    stream: &mut S,
    needle: &str,
    timeout: std::time::Duration,
) -> usize
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    use futures::StreamExt;
    let mut buf = String::new();
    let mut hits = 0usize;
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(chunk.as_ref()));
                while let Some(idx) = buf.find(needle) {
                    hits += 1;
                    buf = buf[idx + needle.len()..].to_string();
                }
            }
            _ => break,
        }
    }
    hits
}

async fn drain_sse_backlog<S, B, E>(stream: &mut S)
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    let _ = collect_sse_matching(stream, "event:", std::time::Duration::from_millis(300)).await;
}

// --- tests -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweep_refreshes_a_drifted_review_and_degrades_a_failed_row_independently() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, bare_tmp, dir, shas) = fixture_multi_pr_repo(&[42, 43]);
    let bare_dir = bare_tmp.path().join("origin.git");
    let sha_42_initial = shas[&42].clone();

    let head_42 = Arc::new(Mutex::new(sha_42_initial.clone()));
    let head_42_route = head_42.clone();
    let gh_router = Router::new()
        .route(
            "/repos/acme/widget/pulls/42",
            get(move || {
                let head = head_42_route.lock().unwrap().clone();
                async move { Json(pull_json(42, &head, "open", false)) }
            }),
        )
        .route(
            &format!("/repos/acme/widget/commits/{sha_42_initial}/check-runs"),
            get(|| async { Json(checks_json(1)) }),
        )
        .route(
            "/repos/acme/widget/pulls/43",
            get(|| async { axum::http::StatusCode::FORBIDDEN }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_for(&dir, gh_addr)).await;
    let client = reqwest::Client::new();

    let id42 = bind_pr(&client, &base, 42).await;
    let id43 = bind_pr(&client, &base, 43).await;

    // GitHub-side push after binding — the local pr ref lands the new
    // commit too (see `push_new_pr_head`'s own doc), and the mock now
    // reports the new head.
    let new_sha_42 = push_new_pr_head(&dir, &bare_dir, 42);
    *head_42.lock().unwrap() = new_sha_42.clone();
    // Re-register isn't possible on the same router — instead the sweep's
    // check-runs call for the NEW sha must resolve too; add it via a
    // second mock server merged in by re-binding both PRs' routers is
    // avoidable here since check-runs failures degrade to `checks: null`
    // only (not `unavailable_reason`) — this test asserts on `head_drift`/
    // `new_head_commits`, not `checks`, so an un-mocked new-sha check-runs
    // 404 is an acceptable, asserted-around degrade.

    let (status, body) = sweep(&client, &base, serde_json::json!({ "all_repos": true })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["summary"]["swept"], 2);
    assert_eq!(
        body["summary"]["refreshed"], 1,
        "only review 42 actually changed"
    );
    assert_eq!(body["summary"]["unavailable"], 1);

    let row42 = row_for(&body, id42);
    assert!(row42["unavailable_reason"].is_null(), "{row42}");
    assert_eq!(row42["pr_state"], "open");
    assert_eq!(row42["head_drift"], true);
    assert_eq!(
        row42["new_head_commits"], 1,
        "one new commit landed on top of the old head"
    );

    let row43 = row_for(&body, id43);
    assert!(!row43["unavailable_reason"].is_null(), "{row43}");
    assert_eq!(row43["pr_state"], serde_json::Value::Null);
    assert_eq!(row43["head_drift"], serde_json::Value::Null);
    assert_eq!(row43["suggest_close"], false);

    // The stored snapshot for review 42 actually moved.
    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id42}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(show["pr_head_sha"], new_sha_42);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweep_flags_suggest_close_for_a_merged_pr_but_never_closes_it() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, shas) = fixture_multi_pr_repo(&[44]);
    let sha = shas[&44].clone();

    let gh_router = Router::new()
        .route(
            "/repos/acme/widget/pulls/44",
            get(move || {
                let sha = sha.clone();
                async move { Json(pull_json(44, &sha, "closed", true)) }
            }),
        )
        .route(
            &format!("/repos/acme/widget/commits/{}/check-runs", shas[&44]),
            get(|| async { Json(checks_json(1)) }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_for(&dir, gh_addr)).await;
    let client = reqwest::Client::new();

    let id = bind_pr(&client, &base, 44).await;
    let (status, body) = sweep(&client, &base, serde_json::json!({ "repo": "fixture" })).await;
    assert_eq!(status, 200, "{body}");
    let row = row_for(&body, id);
    assert_eq!(row["pr_state"], "merged");
    assert_eq!(row["suggest_close"], true);

    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        show["state"], "open",
        "suggest_close is informational only — sweep never writes reviews.state"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweep_emits_review_changed_pr_refreshed_only_when_the_snapshot_actually_changes() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, shas) = fixture_multi_pr_repo(&[45]);
    let sha = shas[&45].clone();
    let check_count = Arc::new(Mutex::new(1usize));
    let check_count_route = check_count.clone();

    let sha_for_pull = sha.clone();
    let sha_for_checks = sha.clone();
    let gh_router = Router::new()
        .route(
            "/repos/acme/widget/pulls/45",
            get(move || {
                let sha = sha_for_pull.clone();
                async move { Json(pull_json(45, &sha, "open", false)) }
            }),
        )
        .route(
            &format!("/repos/acme/widget/commits/{sha_for_checks}/check-runs"),
            get(move || {
                let n = *check_count_route.lock().unwrap();
                async move { Json(checks_json(n)) }
            }),
        );
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_for(&dir, gh_addr)).await;
    let client = reqwest::Client::new();

    let id = bind_pr(&client, &base, 45).await;

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), 200);
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    // First sweep: GitHub reports the SAME head + SAME checks the bind
    // already stored -> nothing changed -> no SSE.
    let (status, body) = sweep(&client, &base, serde_json::json!({ "repo": "fixture" })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["summary"]["refreshed"], 0);
    let hits = collect_sse_matching(
        &mut stream,
        "\"reason\":\"pr_refreshed\"",
        std::time::Duration::from_millis(300),
    )
    .await;
    assert_eq!(
        hits, 0,
        "an unchanged snapshot must not emit review.changed"
    );

    // The checks list changes (a new check-run landed) -> meta_json
    // differs even though the head sha didn't move.
    *check_count.lock().unwrap() = 2;
    let (status, body) = sweep(&client, &base, serde_json::json!({ "repo": "fixture" })).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["summary"]["refreshed"], 1);
    let row = row_for(&body, id);
    assert_eq!(row["head_drift"], false, "the head sha itself did not move");
    let hits = collect_sse_matching(
        &mut stream,
        "\"reason\":\"pr_refreshed\"",
        std::time::Duration::from_millis(400),
    )
    .await;
    assert!(
        hits >= 1,
        "a changed snapshot must emit review.changed{{reason:pr_refreshed}}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweep_requires_exactly_one_of_repo_or_all_repos() {
    let _guard = SERIAL.lock().await;
    let (_repo_tmp, _bare_tmp, dir, _shas) = fixture_multi_pr_repo(&[]);
    let gh_router = Router::new();
    let (gh_addr, _gh_server) = mock_github_server(gh_router).await;
    let (_tmp, base) = boot(cfg_for(&dir, gh_addr)).await;
    let client = reqwest::Client::new();

    let (status, _body) = sweep(&client, &base, serde_json::json!({})).await;
    assert_eq!(status, 400, "neither repo nor all_repos");

    let (status, _body) = sweep(
        &client,
        &base,
        serde_json::json!({ "repo": "fixture", "all_repos": true }),
    )
    .await;
    assert_eq!(status, 400, "both repo and all_repos");
}
