//! V80-M0 — `PUT`/`DELETE /api/annotations/{id}/review` (bind, rebind,
//! unbind an EXISTING annotation's review scope after the fact) plus the
//! `in_diff` caption on `GET /api/reviews/{id}/comments`. Boot pattern
//! mirrors `review_comments.rs`; the SSE helpers mirror
//! `tests/annotations_route.rs`'s own (a small, deliberately duplicated
//! per-file convention already used across this crate's test suite).

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;

use crate::common::git;

async fn boot_with_repos(
    entries: &[(&str, &Path)],
    review: ReviewSection,
) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: entries
            .iter()
            .map(|(name, path)| RepoEntry {
                name: name.to_string(),
                path: std::fs::canonicalize(path).unwrap(),
            })
            .collect(),
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        review,
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

async fn boot_with_repo(
    name: &str,
    path: &Path,
    review: ReviewSection,
) -> (tempfile::TempDir, String) {
    boot_with_repos(&[(name, path)], review).await
}

fn feature_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.rs"), "alpha unique line\nbravo unique line\n").unwrap();
    std::fs::write(dir.join("b.rs"), "charlie unique line\ndelta unique line\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    // Only a.rs changes on feature — b.rs is untouched, so its diff-file
    // set membership differs (the `in_diff` fixture).
    std::fs::write(
        dir.join("a.rs"),
        "alpha unique line\nbravo unique line\nfeature addition\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature"]);
    tmp
}

async fn create_review(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
    head: &str,
    base_ref: &str,
) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": head,
            "base_ref": base_ref,
            "title": "bind test",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn set_review_state(client: &reqwest::Client, base: &str, id: i64, state: &str) {
    let resp = client
        .patch(format!("{base}/api/reviews/{id}"))
        .json(&serde_json::json!({ "state": state }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
}

async fn create_plain_annotation(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
    path: &str,
    line: u32,
    body: &str,
) -> serde_json::Value {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": repo,
            "path": path,
            "line": line,
            "body": body,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json().await.unwrap()
}

async fn bind(
    client: &reqwest::Client,
    base: &str,
    id: &str,
    review_id: i64,
    ps: Option<i64>,
    side: Option<&str>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let mut payload = serde_json::json!({ "review_id": review_id });
    if let Some(p) = ps {
        payload["ps"] = serde_json::json!(p);
    }
    if let Some(s) = side {
        payload["side"] = serde_json::json!(s);
    }
    let resp = client
        .put(format!("{base}/api/annotations/{id}/review"))
        .json(&payload)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

async fn unbind(
    client: &reqwest::Client,
    base: &str,
    id: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .delete(format!("{base}/api/annotations/{id}/review"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

async fn collect_sse<S, B, E>(stream: &mut S, timeout: std::time::Duration) -> String
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    use futures::StreamExt;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => buf.push_str(&String::from_utf8_lossy(chunk.as_ref())),
            _ => break,
        }
    }
    buf
}

/// (1) A plain, unscoped working-tree annotation can be BOUND onto an
/// existing review after the fact — the only pre-V80-M0 way to set
/// `review_id` was at create time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_scopes_a_plain_annotation_onto_a_review() {
    let repo_tmp = feature_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let created = create_plain_annotation(&client, &base, "r", "a.rs", 1, "plain note").await;
    assert!(created["review_id"].is_null());
    let id = created["id"].as_str().unwrap().to_string();

    let review_id = create_review(&client, &base, "r", "feature", "main").await;

    let (status, view) = bind(&client, &base, &id, review_id, None, None).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{view}");
    assert_eq!(view["review_id"], review_id);
    assert_eq!(view["ps_number"], 1);
    assert_eq!(view["side"], "new");

    // Shows up in the review's own Room.
    let resp = client
        .get(format!("{base}/api/reviews/{review_id}/comments"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let listed: serde_json::Value = resp.json().await.unwrap();
    let group = listed["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["path"] == "a.rs")
        .expect("a.rs group");
    assert_eq!(group["comments"][0]["id"], id);
    assert_eq!(group["comments"][0]["body"], "plain note");
}

/// (2) Rebinding onto a DIFFERENT review emits TWO `annotation.changed`
/// SSE frames — one naming the NEW review, one naming the OLD — so both
/// Rooms invalidate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebind_moves_between_reviews_and_emits_both_sse_events() {
    let repo_tmp = feature_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let review_a = create_review(&client, &base, "r", "feature", "main").await;
    let review_b = create_review(&client, &base, "r", "feature", "main").await;

    let created = create_plain_annotation(&client, &base, "r", "a.rs", 1, "moving note").await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, view) = bind(&client, &base, &id, review_a, None, None).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{view}");
    assert_eq!(view["review_id"], review_a);

    // Connect the SSE stream AFTER the first bind so only the REBIND's
    // events are observed.
    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), reqwest::StatusCode::OK);
    let mut stream = sse_resp.bytes_stream();
    // `GET /api/events` replays the bus ring buffer from cursor 0 for a
    // fresh connection, so drain that backlog (the create + first bind
    // already emitted) before asserting on ONLY the rebind's own frames.
    let _ = collect_sse(&mut stream, std::time::Duration::from_millis(300)).await;

    let (status, view) = bind(&client, &base, &id, review_b, None, None).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{view}");
    assert_eq!(view["review_id"], review_b);

    let buf = collect_sse(&mut stream, std::time::Duration::from_secs(10)).await;
    assert!(
        buf.contains(&format!("\"review_id\":{review_b}")),
        "expected the NEW review's id in an SSE frame: {buf}"
    );
    assert!(
        buf.contains(&format!("\"review_id\":{review_a}")),
        "expected the OLD review's id in a SECOND SSE frame: {buf}"
    );
}

/// (3) Unbind clears the scope and is idempotent (a second unbind still
/// 200s).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unbind_clears_scope_and_is_idempotent() {
    let repo_tmp = feature_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let review_id = create_review(&client, &base, "r", "feature", "main").await;
    let created = create_plain_annotation(&client, &base, "r", "a.rs", 1, "bound note").await;
    let id = created["id"].as_str().unwrap().to_string();
    bind(&client, &base, &id, review_id, None, None).await;

    let (status, view) = unbind(&client, &base, &id).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{view}");
    assert!(view["review_id"].is_null());
    assert!(view["ps_number"].is_null());
    assert!(view["side"].is_null());

    let (status2, view2) = unbind(&client, &base, &id).await;
    assert_eq!(status2, reqwest::StatusCode::OK, "{view2}");
    assert!(view2["review_id"].is_null());
}

/// (4) A REPLY has no review scope of its own — bind/unbind 400 naming
/// `parent_id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_and_unbind_a_reply_400_names_parent_id() {
    let repo_tmp = feature_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let review_id = create_review(&client, &base, "r", "feature", "main").await;
    let parent = create_plain_annotation(&client, &base, "r", "a.rs", 1, "parent").await;
    let parent_id = parent["id"].as_str().unwrap().to_string();
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "a.rs", "body": "a reply",
            "parent_id": parent_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let reply: serde_json::Value = resp.json().await.unwrap();
    let reply_id = reply["id"].as_str().unwrap().to_string();

    let (status, body) = bind(&client, &base, &reply_id, review_id, None, None).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("parent_id"),
        "{body}"
    );

    let (status, body) = unbind(&client, &base, &reply_id).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("parent_id"),
        "{body}"
    );
}

/// (5) A review belonging to a DIFFERENT repo is a 4xx.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_wrong_repo_review_is_4xx() {
    let repo_a = feature_repo();
    let repo_b = feature_repo();
    let (_daemon, base) = boot_with_repos(
        &[("repo-a", repo_a.path()), ("repo-b", repo_b.path())],
        ReviewSection::default(),
    )
    .await;
    let client = reqwest::Client::new();

    let review_on_b = create_review(&client, &base, "repo-b", "feature", "main").await;
    let created = create_plain_annotation(&client, &base, "repo-a", "a.rs", 1, "on repo-a").await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, body) = bind(&client, &base, &id, review_on_b, None, None).await;
    assert!(status.is_client_error(), "{status}: {body}");
    assert!(
        body["error"].as_str().unwrap().contains("belongs to repo"),
        "{body}"
    );
}

/// (6) A CLOSED review refuses a new bind — 409, `urn:kb:errors:
/// review-closed` (unlike create, which has no such check — see
/// `resolve_review_bind_scope`'s doc for why bind alone gets one).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_closed_review_is_409() {
    let repo_tmp = feature_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let review_id = create_review(&client, &base, "r", "feature", "main").await;
    set_review_state(&client, &base, review_id, "closed").await;
    let created = create_plain_annotation(&client, &base, "r", "a.rs", 1, "note").await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, body) = bind(&client, &base, &id, review_id, None, None).await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT, "{body}");
    assert_eq!(body["type"], "urn:kb:errors:review-closed");
}

/// (7) Binding never refuses because the path/line is absent at the
/// target patchset's pinned sha — that resolves lazily as an honest
/// orphan on the NEXT `GET /api/reviews/{id}/comments`, never as a bind
/// refusal ("a wrong line is worse than an honest orphan").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bind_where_the_path_is_absent_at_the_pinned_sha_is_200_and_orphaned_on_read() {
    let repo_tmp = feature_repo();
    let dir = repo_tmp.path();
    // An UNTRACKED file — never committed to either `main` or `feature`,
    // so it is absent at BOTH patchset endpoints.
    std::fs::write(
        dir.join("untracked.rs"),
        "ephemeral working-tree-only line\n",
    )
    .unwrap();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let review_id = create_review(&client, &base, "r", "feature", "main").await;
    let created = create_plain_annotation(
        &client,
        &base,
        "r",
        "untracked.rs",
        1,
        "on an untracked file",
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, view) = bind(&client, &base, &id, review_id, None, None).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{view}");

    let resp = client
        .get(format!("{base}/api/reviews/{review_id}/comments"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let listed: serde_json::Value = resp.json().await.unwrap();
    let group = listed["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["path"] == "untracked.rs")
        .expect("untracked.rs group");
    assert_eq!(group["comments"][0]["resolution"]["orphaned"], true);
}

/// (8) `in_diff` is true for a path the patchset's diff touches, false
/// for one it does not — a per-read caption, never a filter (both groups
/// are still listed).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_diff_reports_changed_vs_untouched_files() {
    let repo_tmp = feature_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let review_id = create_review(&client, &base, "r", "feature", "main").await;
    let changed =
        create_plain_annotation(&client, &base, "r", "a.rs", 1, "on the changed file").await;
    let untouched =
        create_plain_annotation(&client, &base, "r", "b.rs", 1, "on the untouched file").await;
    bind(
        &client,
        &base,
        changed["id"].as_str().unwrap(),
        review_id,
        None,
        None,
    )
    .await;
    bind(
        &client,
        &base,
        untouched["id"].as_str().unwrap(),
        review_id,
        None,
        None,
    )
    .await;

    let resp = client
        .get(format!("{base}/api/reviews/{review_id}/comments"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let listed: serde_json::Value = resp.json().await.unwrap();
    let groups = listed["groups"].as_array().unwrap();
    let a_group = groups.iter().find(|g| g["path"] == "a.rs").unwrap();
    let b_group = groups.iter().find(|g| g["path"] == "b.rs").unwrap();
    assert_eq!(a_group["in_diff"], true, "a.rs changed on feature");
    assert_eq!(b_group["in_diff"], false, "b.rs is untouched");
}

/// (9) The batch equivalents (`bind_review`/`unbind_review` ops) share
/// the SAME validation and land in the SAME one-tx/one-SSE batch machinery
/// as every other `AnnotationBatchOp`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_bind_and_unbind_review_ops() {
    let repo_tmp = feature_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let review_id = create_review(&client, &base, "r", "feature", "main").await;
    let created = create_plain_annotation(&client, &base, "r", "a.rs", 1, "batch note").await;
    let id = created["id"].as_str().unwrap().to_string();

    let resp = client
        .post(format!("{base}/api/annotations/batch"))
        .json(&serde_json::json!({
            "repo": "r",
            "ops": [
                { "op": "bind_review", "id": id, "review_id": review_id },
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let report: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(report["changed"], true);

    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "a.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    let view = listed["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id)
        .unwrap();
    assert_eq!(view["review_id"], review_id);

    let resp = client
        .post(format!("{base}/api/annotations/batch"))
        .json(&serde_json::json!({
            "repo": "r",
            "ops": [
                { "op": "unbind_review", "id": id },
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let report: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(report["changed"], true);

    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "a.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    let view = listed["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id)
        .unwrap();
    assert!(view["review_id"].is_null());
}
