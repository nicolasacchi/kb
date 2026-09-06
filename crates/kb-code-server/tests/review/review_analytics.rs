//! PRR-R9 ("The PR Room," kb v0.39 T2, design-addendum-2 §C) — end-to-end
//! HTTP tests for `GET /api/reviews/analytics`. Boot pattern duplicated
//! from `review_findings.rs` (see `review_routes.rs`'s own doc for why
//! each e2e file in this crate keeps its own small helper set) — no
//! GitHub mock needed, this route never calls out.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
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
        review: ReviewSection::default(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("order.rb"), "class Order\nend\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("order.rb"), "class Order\n  def go!\n  end\nend\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature work"]);
    tmp
}

async fn create_review(client: &reqwest::Client, base: &str, repo: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": "feature",
            "base_ref": "main",
            "title": "findings",
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

fn finding(slug: &str, severity: &str, category: &str, path: &str) -> serde_json::Value {
    serde_json::json!({
        "slug": slug,
        "severity": severity,
        "category": category,
        "location": {"path": path, "kind": "single", "lines": [1]},
        "title": format!("finding {slug}"),
        "rationale": "seeded by test",
    })
}

async fn import(client: &reqwest::Client, base: &str, id: i64, findings: Vec<serde_json::Value>) {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/import"))
        .json(&serde_json::json!({ "schema": "kbc-findings/1", "findings": findings }))
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

async fn set_disposition(
    client: &reqwest::Client,
    base: &str,
    id: i64,
    slug: &str,
    disposition: &str,
) {
    let resp = client
        .put(format!(
            "{base}/api/reviews/{id}/findings/{slug}/disposition"
        ))
        .json(&serde_json::json!({ "disposition": disposition }))
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

async fn analytics(
    client: &reqwest::Client,
    base: &str,
    query: &[(&str, &str)],
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .get(format!("{base}/api/reviews/analytics"))
        .query(query)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json().await.unwrap();
    (status, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analytics_aggregates_matrix_and_acceptance_and_publish_and_latency() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    import(
        &client,
        &base,
        id,
        vec![
            finding("f-a", "blocker", "Security", "order.rb"),
            finding("f-b", "blocker", "Security", "order.rb"),
            finding("f-c", "concern", "Style", "other.rb"),
        ],
    )
    .await;
    set_disposition(&client, &base, id, "f-a", "agree").await;
    set_disposition(&client, &base, id, "f-b", "dispute").await;
    // f-c stays undecided.

    let (status, body) = analytics(&client, &base, &[("repo", "r")]).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["schema"], "review-analytics/1");
    assert_eq!(body["total_findings"], 3);
    assert_eq!(body["superseded_count"], 0);

    let matrix = body["by_severity_disposition"].as_array().unwrap();
    assert_eq!(
        matrix.len(),
        3 * 5,
        "every severity x disposition cell is present"
    );
    let blocker_agree = matrix
        .iter()
        .find(|c| c["severity"] == "blocker" && c["disposition"] == "agree")
        .unwrap();
    assert_eq!(blocker_agree["count"], 1);

    let acceptance = body["acceptance"].as_array().unwrap();
    let blocker = acceptance
        .iter()
        .find(|a| a["severity"] == "blocker")
        .unwrap();
    assert_eq!(blocker["accepted"], 1);
    assert_eq!(blocker["rejected"], 1);
    assert_eq!(blocker["rate"], 0.5);
    let ok = acceptance.iter().find(|a| a["severity"] == "ok").unwrap();
    assert_eq!(
        ok["rate"],
        serde_json::Value::Null,
        "a severity with zero findings must report a null rate, never 0"
    );

    assert_eq!(body["publish"]["published"], 0);
    assert_eq!(body["publish"]["unpublished"], 3);

    // f-c is undecided (no disposition_at) -> latency n excludes it; f-a/f-b
    // were disposed at "now", so n=2 with a tiny (possibly 0s) latency.
    assert_eq!(body["latency"]["n"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analytics_recurrence_requires_two_distinct_reviews_and_excludes_singletons() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let id1 = create_review(&client, &base, "r").await;
    let id2 = create_review(&client, &base, "r").await;
    import(
        &client,
        &base,
        id1,
        vec![finding("f-1", "blocker", "Security", "order.rb")],
    )
    .await;
    // Both findings for id2 in ONE import call — the route's default
    // `mode: "full"` reconciles a review's ENTIRE finding set against
    // each call's batch, so a second full-mode import with only f-3
    // would supersede f-2 out from under this same review (see
    // `import_findings_route`'s own doc). f-3 is the singleton
    // category/path pair that must NOT appear in recurrence.
    import(
        &client,
        &base,
        id2,
        vec![
            finding("f-2", "blocker", "Security", "order.rb"),
            finding("f-3", "ok", "Style", "solo.rb"),
        ],
    )
    .await;

    let (status, body) = analytics(&client, &base, &[("repo", "r")]).await;
    assert_eq!(status, 200, "{body}");
    let recurrence = body["recurrence"].as_array().unwrap();
    assert_eq!(recurrence.len(), 1);
    assert_eq!(recurrence[0]["category"], "Security");
    assert_eq!(recurrence[0]["location_path"], "order.rb");
    assert_eq!(recurrence[0]["review_count"], 2);
    let mut ids: Vec<i64> = recurrence[0]["review_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    ids.sort_unstable();
    let mut expected = vec![id1, id2];
    expected.sort_unstable();
    assert_eq!(ids, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analytics_is_deterministic_over_repeated_calls() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;
    import(
        &client,
        &base,
        id,
        vec![
            finding("f-a", "blocker", "Security", "order.rb"),
            finding("f-b", "concern", "Style", "other.rb"),
        ],
    )
    .await;

    let (status1, body1) = analytics(&client, &base, &[("repo", "r")]).await;
    let (status2, body2) = analytics(&client, &base, &[("repo", "r")]).await;
    assert_eq!(status1, 200);
    assert_eq!(status2, 200);
    assert_eq!(
        body1, body2,
        "same underlying rows -> byte-identical response"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analytics_404s_on_an_unknown_repo() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();
    let (status, _body) = analytics(&client, &base, &[("repo", "nope")]).await;
    assert_eq!(status, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn analytics_from_to_window_filters_by_created_at() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;
    import(
        &client,
        &base,
        id,
        vec![finding("f-a", "blocker", "Security", "order.rb")],
    )
    .await;

    let far_future = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 86_400) as i64;
    let (status, body) = analytics(
        &client,
        &base,
        &[("repo", "r"), ("from", &far_future.to_string())],
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["total_findings"], 0,
        "a from-window entirely after the finding's created_at excludes it"
    );
}
