//! S2-A ("One Inbox," kb-code v6.0, design doc `/tmp/design-s2.md` § S2-A)
//! — end-to-end HTTP tests for `GET /api/inbox` (`crate::unified_inbox`).
//! Boot pattern mirrors `review_inbox_timeline.rs` (this crate's own
//! precedent for the same reason: each e2e file duplicates its own small
//! helper set rather than sharing one across files). The pure
//! composition/mapping logic already has unit coverage in
//! `crate::unified_inbox`/`crate::store` themselves — these tests exist to
//! prove the HTTP+store+kb-federation WIRING is correct end to end.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

async fn boot(repos: Vec<RepoEntry>, kb_daemon: KbDaemonSection) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos,
        kb_daemon,
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

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: "http://127.0.0.1:0".to_string(),
        token_file: None,
        public_url: None,
    }
}

fn fixture_repo(seed: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.rb"), format!("class {seed}\nend\n")).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("a.rb"),
        format!("class {seed}\n  def x\n  end\nend\n"),
    )
    .unwrap();
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
            "title": format!("review on {repo}"),
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

async fn import_one_finding(client: &reqwest::Client, base: &str, id: i64, slug: &str) {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/import"))
        .json(&serde_json::json!({
            "schema": "kbc-findings/1",
            "findings": [{
                "slug": slug,
                "severity": "concern",
                "category": "style",
                "location": {"path": "a.rb", "kind": "whole_file"},
                "title": "t",
                "rationale": "r",
            }],
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
}

/// A review-level ("general") question annotation — `path=""`,
/// `anchor_kind="review"` (same shape `review_inbox_timeline.rs`'s own
/// `ask` helper uses). Used here ONLY to prove the unified inbox's
/// annotations lane does NOT double-show a review-scoped thread.
async fn ask_review_scoped(client: &reqwest::Client, base: &str, id: i64, body: &str) -> String {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "",
            "anchor_kind": "review",
            "review_id": id,
            "intent": "question",
            "body": body,
            "author": "you",
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
        .as_str()
        .unwrap()
        .to_string()
}

/// An ordinary working-tree (`review_id` absent) annotation, anchored at
/// `line` in `a.rb` — the annotations lane's own candidate row shape.
async fn working_tree_annotation(
    client: &reqwest::Client,
    base: &str,
    line: u32,
    intent: &str,
    body: &str,
) -> String {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "a.rb",
            "line": line,
            "intent": intent,
            "body": body,
            "author": "you",
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
        .as_str()
        .unwrap()
        .to_string()
}

// --- GET /api/inbox — reviews + annotations lanes -----------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_inbox_composes_reviews_and_annotations_lanes_and_filters_intent_and_scope() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R1");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base) = boot(
        vec![RepoEntry {
            name: "r".to_string(),
            path: dir,
        }],
        disabled_kb_daemon(),
    )
    .await;
    let client = reqwest::Client::new();

    // Reviews lane fodder — one unresolved finding.
    let review_id = create_review(&client, &base, "r").await;
    import_one_finding(&client, &base, review_id, "f-1").await;

    // Annotations lane fodder.
    working_tree_annotation(&client, &base, 1, "question", "why does this class exist?").await;
    working_tree_annotation(&client, &base, 2, "flag-for-agent", "please look at this").await;
    // Wrong intent — must NOT appear.
    working_tree_annotation(&client, &base, 3, "note", "just a note").await;
    // Review-scoped — must NOT appear in the annotations lane (it is
    // already counted inside the reviews lane's own `unanswered_questions`).
    ask_review_scoped(&client, &base, review_id, "a review-level question").await;

    let body: serde_json::Value = client
        .get(format!("{base}/api/inbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "unified-inbox/1");

    let reviews = body["reviews"].as_array().unwrap();
    assert_eq!(reviews.len(), 1);
    assert_eq!(reviews[0]["review_id"], review_id);
    assert_eq!(reviews[0]["unresolved_findings"], 1);
    // The review-scoped question is counted HERE, not in `annotations`.
    assert_eq!(reviews[0]["unanswered_questions"], 1);

    let annotations = body["annotations"].as_array().unwrap();
    let intents: Vec<&str> = annotations
        .iter()
        .map(|a| a["intent"].as_str().unwrap())
        .collect();
    assert_eq!(annotations.len(), 2, "note + review-scoped both excluded");
    assert!(intents.contains(&"question"));
    assert!(intents.contains(&"flag-for-agent"));
    assert!(
        !intents.contains(&"note"),
        "wrong-intent row leaked through"
    );

    let question_row = annotations
        .iter()
        .find(|a| a["intent"] == "question")
        .unwrap();
    assert_eq!(question_row["repo"], "r");
    assert_eq!(question_row["path"], "a.rb");
    assert_eq!(question_row["line"], 1);
    assert_eq!(question_row["author"], "you");
    assert_eq!(question_row["reply_count"], 0);
    assert!(question_row["excerpt"]
        .as_str()
        .unwrap()
        .starts_with("why does this class exist?"));
    assert!(question_row["updated_at"].as_i64().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_inbox_annotations_lane_is_newest_updated_at_first() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R2");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base) = boot(
        vec![RepoEntry {
            name: "r".to_string(),
            path: dir,
        }],
        disabled_kb_daemon(),
    )
    .await;
    let client = reqwest::Client::new();

    let first = working_tree_annotation(&client, &base, 1, "question", "first").await;
    // `updated_at` is whole unix SECONDS (`chrono::Utc::now().timestamp()`)
    // — sleep past a second boundary so the two rows are guaranteed
    // distinguishable by timestamp rather than racing the sort's `id`
    // tiebreak (which has no relation to creation order).
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let second = working_tree_annotation(&client, &base, 2, "question", "second").await;

    let body: serde_json::Value = client
        .get(format!("{base}/api/inbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = body["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], second, "the more-recently-created row sorts first");
    assert_eq!(ids[1], first);
}

// --- GET /api/inbox — kb lane degradation --------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_inbox_kb_lane_reports_disabled_and_other_lanes_still_render() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R3");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base) = boot(
        vec![RepoEntry {
            name: "r".to_string(),
            path: dir,
        }],
        disabled_kb_daemon(),
    )
    .await;
    let client = reqwest::Client::new();
    working_tree_annotation(&client, &base, 1, "question", "q").await;

    let resp = client
        .get(format!("{base}/api/inbox"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "never a 500");
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["kb"]["available"], false);
    assert_eq!(body["kb"]["reason"], "disabled");
    assert!(body["kb"]["desk"].is_null());
    assert!(body["kb"]["comments"].is_null());
    // The other two lanes render regardless.
    assert_eq!(body["annotations"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_inbox_kb_lane_reports_unreachable_when_the_kb_daemon_is_down() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R4");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base) = boot(
        vec![RepoEntry {
            name: "r".to_string(),
            path: dir,
        }],
        KbDaemonSection {
            enabled: true,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
    )
    .await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/inbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["kb"]["available"], false);
    assert_eq!(body["kb"]["reason"], "unreachable");
    assert!(body["kb"]["desk"].is_null());
    assert!(body["kb"]["comments"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_inbox_kb_lane_reports_sibling_mismatch() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R5");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();

    let mock = axum::Router::new().route(
        "/api/identity",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "name": "kb",
                "sibling_protocol": "kb-sibling/9",
                "sibling_major": 9,
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    let _mock_task = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });

    let (_tmp, base) = boot(
        vec![RepoEntry {
            name: "r".to_string(),
            path: dir,
        }],
        KbDaemonSection {
            enabled: true,
            url: format!("http://{mock_addr}"),
            token_file: None,
            public_url: None,
        },
    )
    .await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/inbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["kb"]["available"], false);
    assert_eq!(body["kb"]["reason"], "sibling_mismatch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unified_inbox_kb_lane_relays_and_truncates_at_fifty() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R6");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();

    // 51 desk items and 51 comment items — one more than the lane cap, so
    // the route's own post-fetch truncation must be exercised (not just
    // relying on kb's own default page size).
    let desk_items: Vec<serde_json::Value> = (0..51)
        .map(|i| {
            serde_json::json!({
                "kb": "memory",
                "id": format!("d{i}"),
                "source_relative": format!("handoff/{i}.html"),
                "title": format!("draft {i}"),
                "updated_unix": 1_700_000_000_i64,
                "comments_open": 0,
                "comments_total": 0,
                "read_state": "unread",
                "changed_since_read": false,
            })
        })
        .collect();
    let comment_items: Vec<serde_json::Value> = (0..51)
        .map(|i| {
            serde_json::json!({
                "kb": "memory",
                "artifact_id": format!("a{i}"),
                "title": format!("comment {i}"),
                "comment_id": format!("c{i}"),
                "excerpt": "why here?",
                "author": "you",
                "reply_count": 0,
                "anchor": "selection",
                "stale": false,
                "created_at": 1_700_000_000_i64,
                "updated_at": 1_700_000_000_i64,
            })
        })
        .collect();

    let mock = axum::Router::new()
        .route(
            "/api/desk",
            axum::routing::get(move || {
                let items = desk_items.clone();
                async move { axum::Json(serde_json::json!({"items": items, "attention": 3})) }
            }),
        )
        .route(
            "/api/inbox",
            axum::routing::get(move || {
                let items = comment_items.clone();
                async move { axum::Json(serde_json::json!({"items": items, "total_open": 51})) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = listener.local_addr().unwrap();
    let _mock_task = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });

    let (_tmp, base) = boot(
        vec![RepoEntry {
            name: "r".to_string(),
            path: dir,
        }],
        KbDaemonSection {
            enabled: true,
            url: format!("http://{mock_addr}"),
            token_file: None,
            public_url: None,
        },
    )
    .await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/inbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["kb"]["available"], true);
    assert!(body["kb"]["reason"].is_null());
    assert_eq!(body["kb"]["desk"]["items"].as_array().unwrap().len(), 50);
    assert_eq!(body["kb"]["desk"]["attention"], 3);
    assert_eq!(body["kb"]["desk"]["truncated"], true);
    assert_eq!(
        body["kb"]["comments"]["items"].as_array().unwrap().len(),
        50
    );
    assert_eq!(body["kb"]["comments"]["total_open"], 51);
    assert_eq!(body["kb"]["comments"]["truncated"], true);
}
