//! V3.R1 — end-to-end HTTP tests for local review sessions
//! (`/api/reviews…`). Boot pattern mirrors `tests/reading_sets_route.rs`
//! / `tests/checkout_route.rs`. Fixture repo has `main` + `feature`.
//!
//! Note: `tests/review_routes.rs` already owns Phase G (merge-check /
//! range-diff / GitHub PRs); this file is the local-sessions suite.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
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

fn fixture_feature_branch() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature-1\n").unwrap();
    std::fs::write(dir.join("b.txt"), "new\n").unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_snapshot_interdiff_viewed_stale_delete() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();

    // Create → ps1 + ref exists
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "r",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "my feature",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["latest_ps"], 1);

    let show_ref = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["show-ref", "--verify", &format!("refs/kbc/review/{id}/ps1")])
        .output()
        .unwrap();
    assert!(
        show_ref.status.success(),
        "ps1 ref missing: {}",
        String::from_utf8_lossy(&show_ref.stderr)
    );

    // Amend branch + explicit snapshot → ps2
    std::fs::write(dir.join("a.txt"), "feature-2\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--amend", "-m", "feature work v2"]);

    let resp = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let snap: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(snap["ps_number"], 2);

    // Interdiff files non-empty + range_diff present
    let resp = client
        .get(format!("{base}/api/reviews/{id}/interdiff"))
        .query(&[("from", "1"), ("to", "2")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let inter: serde_json::Value = resp.json().await.unwrap();
    assert!(
        !inter["files"].as_array().unwrap().is_empty(),
        "interdiff files should be non-empty: {inter}"
    );
    assert!(inter["range_diff"]["pairs"].is_array());

    // Viewed set → files shows viewed
    let files_resp = client
        .get(format!("{base}/api/reviews/{id}/files"))
        .send()
        .await
        .unwrap();
    assert_eq!(files_resp.status(), 200);
    let files: serde_json::Value = files_resp.json().await.unwrap();
    let a_file = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "a.txt")
        .expect("a.txt in files");
    let blob = a_file["blob_sha"].as_str().unwrap().to_string();
    assert!(!blob.is_empty());

    let resp = client
        .put(format!("{base}/api/reviews/{id}/viewed"))
        .json(&serde_json::json!({ "path": "a.txt", "blob_sha": blob }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let files: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/files"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let a_file = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "a.txt")
        .unwrap();
    assert_eq!(a_file["viewed"], true);
    assert_eq!(a_file["viewed_stale"], false);

    // Change file + snapshot → viewed_stale=true
    std::fs::write(dir.join("a.txt"), "feature-3\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--amend", "-m", "feature work v3"]);
    let resp = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["ps_number"],
        3
    );

    let files: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/files"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let a_file = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "a.txt")
        .unwrap();
    assert_eq!(a_file["viewed"], true);
    assert_eq!(a_file["viewed_stale"], true);

    // Close stops auto-capture (pure-fn gate is unit-tested; here we just
    // confirm close sticks).
    let resp = client
        .patch(format!("{base}/api/reviews/{id}"))
        .json(&serde_json::json!({ "state": "closed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["state"],
        "closed"
    );

    // DELETE removes all refs
    // re-open path: create a fresh open review to delete with refs
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "r",
            "head_ref": "feature",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap();
    let id2 = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    assert!(Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "show-ref",
            "--verify",
            &format!("refs/kbc/review/{id2}/ps1")
        ])
        .output()
        .unwrap()
        .status
        .success());

    let resp = client
        .delete(format!("{base}/api/reviews/{id2}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert!(!Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "show-ref",
            "--verify",
            &format!("refs/kbc/review/{id2}/ps1")
        ])
        .output()
        .unwrap()
        .status
        .success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn max_patchsets_gcs_oldest() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let dir = repo_tmp.path();
    let review = ReviewSection {
        patchset_capture: false,
        max_patchsets: 3,
        ..ReviewSection::default()
    };
    let (_daemon_tmp, base) = boot_with_repo("r", dir, review).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "r",
            "head_ref": "feature",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // Capture up to ps4 — each amend changes tip
    for i in 2..=4 {
        std::fs::write(dir.join("a.txt"), format!("v{i}\n")).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "--amend", "-m", &format!("v{i}")]);
        let resp = client
            .post(format!("{base}/api/reviews/{id}/snapshot"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ps_number"], i);
    }

    // ps1 GC'd; ps_number monotonic (4 exists, 1 gone)
    let show = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let nums: Vec<i64> = show["patchsets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["ps_number"].as_i64().unwrap())
        .collect();
    assert!(!nums.contains(&1), "ps1 should be GC'd: {nums:?}");
    assert!(nums.contains(&4), "ps4 should remain: {nums:?}");
    assert_eq!(nums.len(), 3);

    assert!(!Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["show-ref", "--verify", &format!("refs/kbc/review/{id}/ps1")])
        .output()
        .unwrap()
        .status
        .success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutation_routes_404_for_non_loopback() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();

    // Same technique as tests/transcripts.rs: XFF spoof with peer=loopback
    // as a trusted hop makes is_loopback_origin return false → 404.
    let resp = client
        .post(format!("{base}/api/reviews"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({
            "repo": "r",
            "head_ref": "feature",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST /api/reviews must 404 a non-loopback caller"
    );

    let resp = client
        .post(format!("{base}/api/reviews/1/snapshot"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_and_show_round_trip() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "r",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "list me",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let list: serde_json::Value = client
        .get(format!("{base}/api/reviews"))
        .query(&[("repo", "r"), ("state", "open")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["reviews"].as_array().unwrap().len(), 1);
    assert_eq!(list["reviews"][0]["id"], id);
    assert!(list["reviews"][0]["files_count"].as_u64().unwrap() >= 1);
    assert!(list["reviews"][0]["verdict"].is_null());
    assert_eq!(list["reviews"][0]["verdict_stale"], false);

    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(show["patchsets"].as_array().unwrap().len(), 1);
    assert!(show["patchsets"][0]["commit_count"].as_u64().unwrap() >= 1);
    assert!(show["verdict"].is_null());
    assert_eq!(show["verdict_stale"], false);
}

/// PF-K1 — `GET /api/reviews` batches `latest_patchset`/`get_review_pr_
/// binding`/`get_review_report`/`list_viewed`/open-annotation-counts
/// across every review in one request (`reviews::compose_review_list_rows`)
/// instead of a sequential per-review fan-out. Three reviews share the
/// SAME (base, tip) — so they share the exact same diff/paths — which is
/// exactly the case a batching bug could smear per-review values across
/// rows: a `viewed` mark on ONE review must not leak into the others, an
/// open annotation on a shared path must count for EVERY review whose diff
/// includes it (not just the review that happens to own the path in some
/// internal map), and a verdict set on ONE review must not appear on the
/// others.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_reviews_batches_across_a_multi_review_fixture_with_correct_per_row_attribution() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let mut ids = Vec::new();
    for title in ["r1", "r2", "r3"] {
        let resp = client
            .post(format!("{base}/api/reviews"))
            .json(&serde_json::json!({
                "repo": "r",
                "head_ref": "feature",
                "base_ref": "main",
                "title": title,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
        ids.push(
            resp.json::<serde_json::Value>().await.unwrap()["id"]
                .as_i64()
                .unwrap(),
        );
    }
    let (id1, id2, id3) = (ids[0], ids[1], ids[2]);

    // Mark a.txt viewed ONLY on review 2.
    let files: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id2}/files"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let blob = files["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "a.txt")
        .unwrap()["blob_sha"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = client
        .put(format!("{base}/api/reviews/{id2}/viewed"))
        .json(&serde_json::json!({ "path": "a.txt", "blob_sha": blob }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // An open working-tree annotation on a.txt — a path EVERY review's
    // diff includes (same base/head across all three) — must count for
    // ALL three rows, not just one.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "a.txt",
            "line": 1,
            "intent": "question",
            "body": "why?",
            "author": "you",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());

    // A verdict set ONLY on review 3.
    let resp = client
        .put(format!("{base}/api/reviews/{id3}/verdict"))
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let list: serde_json::Value = client
        .get(format!("{base}/api/reviews"))
        .query(&[("repo", "r"), ("state", "open")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = list["reviews"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    let row_for = |id: i64| -> &serde_json::Value { rows.iter().find(|r| r["id"] == id).unwrap() };

    // files_count is identical across all three (same base/head diff).
    for id in [id1, id2, id3] {
        assert!(
            row_for(id)["files_count"].as_u64().unwrap() >= 1,
            "review {id} should report a non-empty diff"
        );
    }

    // viewed_count: ONLY review 2 has the mark, and it must not leak.
    assert_eq!(row_for(id1)["viewed_count"], 0, "review 1 unaffected");
    assert_eq!(row_for(id2)["viewed_count"], 1, "review 2 has the mark");
    assert_eq!(row_for(id3)["viewed_count"], 0, "review 3 unaffected");

    // open_annotations: a.txt is in every review's diff, so all three
    // report the SAME count for the SAME shared annotation.
    assert_eq!(row_for(id1)["open_annotations"], 1);
    assert_eq!(row_for(id2)["open_annotations"], 1);
    assert_eq!(row_for(id3)["open_annotations"], 1);

    // verdict: ONLY review 3, and it must not leak either.
    assert!(row_for(id1)["verdict"].is_null());
    assert!(row_for(id2)["verdict"].is_null());
    assert_eq!(row_for(id3)["verdict"]["state"], "approve");
    assert_eq!(row_for(id1)["verdict_stale"], false);
    assert_eq!(row_for(id2)["verdict_stale"], false);
    assert_eq!(row_for(id3)["verdict_stale"], false);
}

#[test]
fn should_auto_capture_pure_fn() {
    use kb_code_server::reviews::should_auto_capture;
    assert!(should_auto_capture(true, true, false, true));
    assert!(!should_auto_capture(false, true, false, true));
    assert!(!should_auto_capture(true, false, false, true));
    assert!(!should_auto_capture(true, true, true, true));
    assert!(!should_auto_capture(true, true, false, false));
}

#[test]
fn default_base_ref_follows_origin_head_else_head() {
    use kb_code_server::reviews::default_base_ref;
    let tmp = fixture_feature_branch();
    let dir = tmp.path();
    // Fixture leaves HEAD on `feature`.
    assert_eq!(
        default_base_ref(dir),
        "feature",
        "no origin/HEAD → HEAD heuristic"
    );

    let main_sha = git_out(dir, &["rev-parse", "main"]);
    git(dir, &["update-ref", "refs/remotes/origin/main", &main_sha]);
    git(
        dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    assert_eq!(
        default_base_ref(dir),
        "main",
        "origin/HEAD wins over HEAD=feature"
    );
}

// silence unused in some builds
#[allow(dead_code)]
fn _tip(dir: &Path) -> String {
    git_out(dir, &["rev-parse", "HEAD"])
}

// --- V4.C2 verdict --------------------------------------------------------

async fn create_open_review(client: &reqwest::Client, base: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "r",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "verdict me",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

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

/// V4.C2 — `GET /api/events` replays the bus ring buffer from cursor 0
/// for a fresh connection, so a stream opened mid-test first delivers
/// every event setup already emitted (review.changed from create/first
/// verdict). Consume that backlog so assertions count only NEW events.
async fn drain_sse_backlog<S, B, E>(stream: &mut S)
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    let _ = collect_sse_matching(stream, "event:", std::time::Duration::from_millis(300)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verdict_set_clear_round_trip_and_stale_after_snapshot() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();
    let id = create_open_review(&client, &base).await;

    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": "approve", "note": "lgtm" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["changed"], true);

    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(show["verdict"]["state"], "approve");
    assert_eq!(show["verdict"]["note"], "lgtm");
    assert_eq!(show["verdict"]["ps"], 1);
    assert_eq!(show["verdict_stale"], false);

    let list: serde_json::Value = client
        .get(format!("{base}/api/reviews"))
        .query(&[("repo", "r")])
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
    assert_eq!(row["verdict"]["state"], "approve");
    assert_eq!(row["verdict_stale"], false);

    // Allowed on closed reviews.
    let resp = client
        .patch(format!("{base}/api/reviews/{id}"))
        .json(&serde_json::json!({ "state": "closed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": "request-changes" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["changed"],
        true
    );

    // New snapshot → verdict_stale.
    std::fs::write(dir.join("a.txt"), "feature-stale\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "--amend", "-m", "v2"]);
    let resp = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(show["verdict"]["ps"], 1);
    assert_eq!(show["verdict_stale"], true);

    let resp = client
        .delete(format!("{base}/api/reviews/{id}/verdict"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["changed"],
        true
    );
    let show: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(show["verdict"].is_null());
    assert_eq!(show["verdict_stale"], false);

    // Second clear is a no-op.
    let resp = client
        .delete(format!("{base}/api/reviews/{id}/verdict"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["changed"],
        false
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verdict_noop_emits_no_sse() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();
    let id = create_open_review(&client, &base).await;

    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": "comment" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["changed"],
        true
    );

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), 200);
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": "comment" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["changed"],
        false
    );

    let hits = collect_sse_matching(
        &mut stream,
        "review.changed",
        std::time::Duration::from_millis(400),
    )
    .await;
    assert_eq!(hits, 0, "identical state+note must not emit SSE");

    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.json::<serde_json::Value>().await.unwrap()["changed"],
        true
    );
    let hits = collect_sse_matching(
        &mut stream,
        "\"reason\":\"verdict\"",
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(hits >= 1, "changed verdict must emit reason=verdict");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verdict_zero_patchset_is_400() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let (daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    let id = store
        .create_review("r", None, "main", "feature", None, 1)
        .unwrap();
    drop(store);

    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "{}", resp.text().await.unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verdict_routes_404_for_non_loopback() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();

    let resp = client
        .put(format!("{base}/api/reviews/1/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "PUT /verdict must 404 a non-loopback caller"
    );

    let resp = client
        .delete(format!("{base}/api/reviews/1/verdict"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "DELETE /verdict must 404 non-loopback");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggestion_apply_404_for_non_loopback() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_feature_branch();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path(), ReviewSection::default()).await;
    let client = reqwest::Client::new();

    // Same XFF spoof as mutation_routes_404_for_non_loopback: peer is
    // loopback (trusted hop) so X-Forwarded-For is consulted and the
    // loopback_only layer 404s.
    let resp = client
        .post(format!("{base}/api/annotations/ann_doesnotexist/apply"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST /api/annotations/{{id}}/apply must 404 a non-loopback caller"
    );
}
