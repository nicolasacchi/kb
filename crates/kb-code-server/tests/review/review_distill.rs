//! CT-E7 — `GET /api/reviews/{id}/distill`. Boot pattern mirrors
//! `review_comments.rs` / `local_review_routes.rs`.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.rs"), "fn one() {}\nfn two() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("a.rs"),
        "fn one() {}\nfn two() {}\nfn three() {}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature work"]);
    tmp
}

async fn boot_with_repo(
    name: &str,
    path: &Path,
    review: ReviewSection,
) -> (tempfile::TempDir, String) {
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

async fn create_review(client: &reqwest::Client, base: &str, repo: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": "feature",
            "base_ref": "main",
            "title": "distill fixture",
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

async fn snapshot(client: &reqwest::Client, base: &str, id: i64) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["ps_number"]
        .as_i64()
        .unwrap()
}

async fn comment_on(
    client: &reqwest::Client,
    base: &str,
    review_id: i64,
    path: &str,
    line: u32,
    body: &str,
) -> serde_json::Value {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": path,
            "line": line,
            "body": body,
            "review_id": review_id,
            "side": "new",
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

async fn resolve_annotation(client: &reqwest::Client, base: &str, id: &str) {
    let resp = client
        .patch(format!("{base}/api/annotations/{id}"))
        .json(&serde_json::json!({ "resolved": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

async fn put_suggestion(
    client: &reqwest::Client,
    base: &str,
    annotation_id: &str,
    replacement: &str,
) {
    let resp = client
        .put(format!("{base}/api/annotations/{annotation_id}/suggestion"))
        .json(&serde_json::json!({ "replacement": replacement }))
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

async fn apply_suggestion(client: &reqwest::Client, base: &str, annotation_id: &str) {
    let resp = client
        .post(format!("{base}/api/annotations/{annotation_id}/apply"))
        .json(&serde_json::json!({}))
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

async fn put_verdict(client: &reqwest::Client, base: &str, id: i64, state: &str, note: &str) {
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": state, "note": note }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

async fn distill(
    client: &reqwest::Client,
    base: &str,
    id: i64,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .get(format!("{base}/api/reviews/{id}/distill"))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

fn strip_generated_at(mut v: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("generated_at");
    }
    v
}

/// Full document over a review fixture with comments, a suggestion, and a
/// verdict — every top-level section is present and correctly wired; a
/// second capture makes the verdict stale; applying the suggestion flips
/// the audit fields; re-distilling twice with nothing changed in between
/// is byte-identical modulo `generated_at` (a pure read).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distill_full_document_comments_verdict_suggestion() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let id = create_review(&client, &base, "r").await;

    let open = comment_on(&client, &base, id, "a.rs", 1, "open thread").await;
    let open_id = open["id"].as_str().unwrap().to_string();
    let done = comment_on(&client, &base, id, "a.rs", 2, "resolved thread").await;
    let done_id = done["id"].as_str().unwrap().to_string();
    resolve_annotation(&client, &base, &done_id).await;

    put_suggestion(&client, &base, &open_id, "fn ONE() {}").await;
    put_verdict(&client, &base, id, "approve", "lgtm").await;

    let (status, body) = distill(&client, &base, id).await;
    assert_eq!(status, reqwest::StatusCode::OK, "{body}");
    assert_eq!(body["schema"], "review-distill/1");

    let review = &body["review"];
    assert_eq!(review["id"], id);
    assert_eq!(review["repo"], "r");
    assert_eq!(review["base_ref"], "main");
    assert_eq!(review["head_ref"], "feature");
    assert_eq!(review["title"], "distill fixture");
    assert_eq!(review["state"], "open");

    let patchsets = body["patchsets"].as_array().unwrap();
    assert_eq!(patchsets.len(), 1);
    assert_eq!(patchsets[0]["ps_number"], 1);
    assert!(patchsets[0]["tip_sha"].as_str().unwrap().len() == 40);
    assert_eq!(body["latest_ps"], 1);

    let files = body["files"].as_array().unwrap();
    assert!(files.iter().any(|f| f["path"] == "a.rs"));

    assert_eq!(body["verdict"]["state"], "approve");
    assert_eq!(body["verdict"]["note"], "lgtm");
    assert_eq!(body["verdict"]["ps"], 1);
    assert_eq!(body["verdict_stale"], false);

    assert_eq!(body["thread_count"], 2);
    assert_eq!(body["unresolved_count"], 1);

    let group = body["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["path"] == "a.rs")
        .expect("a.rs group");
    let comments = group["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2, "distill includes resolved threads too");
    let open_c = comments
        .iter()
        .find(|c| c["body"] == "open thread")
        .unwrap();
    assert_eq!(open_c["resolved"], false);
    assert_eq!(open_c["resolution"]["orphaned"], false);
    assert_eq!(open_c["resolution"]["line"], 1);
    assert_eq!(open_c["suggestion"]["applied"], false);
    let done_c = comments
        .iter()
        .find(|c| c["body"] == "resolved thread")
        .unwrap();
    assert_eq!(done_c["resolved"], true);
    assert!(done_c["suggestion"].is_null());

    let suggestions = body["suggestions"].as_array().unwrap();
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0]["annotation_id"], open_id);
    assert_eq!(suggestions[0]["path"], "a.rs");
    assert_eq!(suggestions[0]["replacement"], "fn ONE() {}");
    assert_eq!(suggestions[0]["applied"], false);
    assert!(suggestions[0]["applied_at"].is_null());
    assert!(suggestions[0]["applied_head_sha"].is_null());
    assert_eq!(body["suggestions_applied_count"], 0);

    // Idempotent: re-distilling with nothing changed yields the same
    // document modulo `generated_at`.
    let (status2, body2) = distill(&client, &base, id).await;
    assert_eq!(status2, reqwest::StatusCode::OK);
    assert_eq!(strip_generated_at(body.clone()), strip_generated_at(body2));

    // Apply the suggestion — the audit trail flips on.
    apply_suggestion(&client, &base, &open_id).await;
    let (_, body3) = distill(&client, &base, id).await;
    let suggestions = body3["suggestions"].as_array().unwrap();
    assert_eq!(suggestions[0]["applied"], true);
    assert!(suggestions[0]["applied_at"].as_i64().is_some());
    let head_sha = suggestions[0]["applied_head_sha"].as_str().unwrap();
    assert_eq!(head_sha.len(), 40);
    assert_eq!(body3["suggestions_applied_count"], 1);

    // A second patchset makes the ps1 verdict stale — the distill route
    // must recompute this against the LATEST ps, not hardcode "false".
    std::fs::write(
        dir.join("a.rs"),
        "fn one() {}\nfn two() {}\nfn three() {}\nfn four() {}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "more feature work"]);
    assert_eq!(snapshot(&client, &base, id).await, 2);

    let (_, body4) = distill(&client, &base, id).await;
    assert_eq!(body4["latest_ps"], 2);
    assert_eq!(body4["patchsets"].as_array().unwrap().len(), 2);
    assert_eq!(body4["verdict"]["ps"], 1, "verdict itself is unchanged");
    assert_eq!(body4["verdict_stale"], true);
}

/// Unknown review id → 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distill_unknown_review_is_404() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/reviews/999999/distill"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

/// A review row with zero patchsets (GC'd to nothing, or never captured —
/// same store-level state either way) is an honest 404, not an empty
/// document — same convention `resolve_ps`/`review_annotations` already
/// use. Store setup mirrors `local_review_routes.rs`'s
/// `verdict_zero_patchset_is_400`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn distill_gcd_review_is_honest_404() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    let id = store
        .create_review("r", None, "main", "feature", None, 1)
        .unwrap();
    drop(store);

    let (status, body) = distill(&client, &base, id).await;
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("no patchsets"),
        "unexpected error body: {body}"
    );
}
