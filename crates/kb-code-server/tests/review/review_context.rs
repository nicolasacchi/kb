//! v0.44 F9b — end-to-end HTTP tests for `GET /api/reviews/{id}/context`
//! (`kbc-review-context/1`) and `GET /api/reviews/{id}/explain-base`
//! (`kbc-review-base-explain/1`) in `crate::review_context`. The assembler
//! (budget cut, ordering, golden) is unit-tested in the module; these prove
//! the WIRING over real git + HTTP: the compositions are the routes' own,
//! the secret denylist holds, a budget cut is named, and two reads are
//! byte-identical.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

async fn boot(repos: Vec<RepoEntry>) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos,
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
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

/// main: lib.rb. feature: edits lib.rb, adds app.rb and a `.env` with a
/// secret value the bundle must never contain.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path();
    git(d, &["init", "-q", "-b", "main"]);
    git(d, &["config", "user.email", "test@example.com"]);
    git(d, &["config", "user.name", "Test"]);
    std::fs::write(d.join("lib.rb"), "def a\n  1\nend\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "base"]);
    git(d, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(d.join("lib.rb"), "def a\n  2\nend\n").unwrap();
    std::fs::write(d.join("app.rb"), "require_relative 'lib'\nputs a\n").unwrap();
    std::fs::write(d.join(".env"), "API_TOKEN=hunter2-super-secret\n").unwrap();
    git(d, &["add", "-A"]);
    git(d, &["commit", "-q", "-m", "feature work"]);
    tmp
}

async fn create_review(client: &reqwest::Client, base: &str, head: &str, title: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "r", "head_ref": head, "base_ref": "main", "title": title,
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

async fn get_text(client: &reqwest::Client, url: String, q: &[(&str, &str)]) -> (u16, String) {
    let resp = client.get(url).query(q).send().await.unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

fn sections(body: &serde_json::Value) -> Vec<String> {
    body["omitted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["section"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bundle_carries_the_routes_own_compositions_and_never_a_denylisted_byte() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: std::fs::canonicalize(repo_tmp.path()).unwrap(),
    }])
    .await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "feature", "ctx").await;

    // A human question and a finding, so threads and findings are non-empty.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "", "anchor_kind": "review", "review_id": id,
            "intent": "question", "body": "why change a to 2?", "author": "you",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/import"))
        .json(&serde_json::json!({
            "schema": "kbc-findings/1",
            "findings": [{
                "slug": "f-magic", "severity": "concern", "category": "style",
                "location": {"path": "lib.rb", "kind": "single", "lines": [2]},
                "title": "magic number", "rationale": "name it",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    let (st, text) = get_text(
        &client,
        format!("{base}/api/reviews/{id}/context"),
        &[("budget", "50000")],
    )
    .await;
    assert_eq!(st, 200, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["schema"], "kbc-review-context/1");
    // v0.45 N8 — the real route body conforms to the registered schema.
    crate::common::assert_conforms("kbc-review-context/1", &body);
    assert_eq!(body["review_id"], id);
    assert_eq!(body["ps"], 1);
    assert_eq!(body["header"]["title"], "ctx");
    assert_eq!(body["header"]["patchset"]["ps"], 1);
    assert!(
        body["header"]["base_resolution"]["summary"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the header says how the base was resolved: {}",
        body["header"]
    );

    // Threads: the open human question (the finding's own thread is NOT here).
    let threads = body["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1, "{threads:?}");
    assert_eq!(threads[0]["body"], "why change a to 2?");
    // Findings: the route's own object (resolution block and all).
    let findings = body["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["slug"], "f-magic");
    assert!(findings[0]["resolution"].is_object(), "{}", findings[0]);

    // No verdict: since does not apply, and says why.
    assert_eq!(body["since"]["applicable"], false);
    assert_eq!(body["since"]["reason"], "no-verdict");

    // The change set and its reading order name the real files.
    let files: Vec<&str> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(
        files.contains(&"lib.rb") && files.contains(&"app.rb"),
        "{files:?}"
    );
    assert!(!body["reading_order"].as_array().unwrap().is_empty());

    // The denylist: `.env` is named, never read.
    assert!(
        !text.contains("hunter2"),
        "a denylisted byte leaked into the bundle"
    );
    let redacted = body["patch"]["redacted"].as_array().unwrap();
    assert!(
        redacted.iter().any(|r| r["path"] == ".env"),
        "redacted: {redacted:?}"
    );
    let patch = body["patch"]["text"].as_str().unwrap();
    assert!(patch.contains("lib.rb") && patch.contains("app.rb"));
    // Reading order: a file the other imports comes first, whichever the
    // order; the point pinned is that the patch text FOLLOWS reading_order.
    let order: Vec<&str> = body["reading_order"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["path"].as_str().unwrap())
        .collect();
    let pos = |p: &str| patch.find(&format!("diff --git a/{p} b/{p}")).unwrap();
    let in_order: Vec<usize> = order
        .iter()
        .filter(|p| **p != ".env" && files.contains(p))
        .map(|p| pos(p))
        .collect();
    assert!(
        in_order.windows(2).all(|w| w[0] < w[1]),
        "patch text must follow reading order {order:?}"
    );
    assert_eq!(body["omitted"], serde_json::json!([]));

    // Deterministic: a second read is byte-identical.
    let (_, again) = get_text(
        &client,
        format!("{base}/api/reviews/{id}/context"),
        &[("budget", "50000")],
    )
    .await;
    assert_eq!(text, again);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_small_budget_cuts_a_leading_part_and_names_every_cut() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: std::fs::canonicalize(repo_tmp.path()).unwrap(),
    }])
    .await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "feature", "ctx").await;
    for i in 0..3 {
        let resp = client
            .post(format!("{base}/api/annotations"))
            .json(&serde_json::json!({
                "repo": "r", "path": "", "anchor_kind": "review", "review_id": id,
                "intent": "question", "body": format!("question number {i} {}", "x".repeat(400)),
                "author": "you",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201);
    }
    // Measure the real header and first thread, then grant room for exactly
    // those (plus a few spare bytes), not for a second thread.
    let (_, full) = get_text(
        &client,
        format!("{base}/api/reviews/{id}/context"),
        &[("budget", "50000")],
    )
    .await;
    let full: serde_json::Value = serde_json::from_str(&full).unwrap();
    assert!(full["omitted"].as_array().unwrap().is_empty());
    let weigh = |v: &serde_json::Value| serde_json::to_string(v).unwrap().len() + 1;
    let wanted = weigh(&full["header"]) + weigh(&full["threads"][0]) + 5;
    let tokens = wanted.div_ceil(4).to_string();
    let (st, text) = get_text(
        &client,
        format!("{base}/api/reviews/{id}/context"),
        &[("budget", tokens.as_str())],
    )
    .await;
    assert_eq!(st, 200, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    let kept = body["threads"].as_array().unwrap().len();
    assert!(
        (1..3).contains(&kept),
        "a prefix of the threads, got {kept}"
    );
    let cut = sections(&body);
    assert!(cut.contains(&"threads".to_string()), "{cut:?}");
    assert!(
        cut.contains(&"files".to_string()) && cut.contains(&"patch".to_string()),
        "every later section is named, not silently absent: {cut:?}"
    );
    let first = &body["omitted"][0];
    assert_eq!(first["section"], "threads");
    assert_eq!(first["reason"], "budget");
    assert_eq!(first["kept"], kept);
    assert_eq!(first["total"], 3);
    assert_eq!(body["patch"]["text"], "");
    // Header always present.
    assert_eq!(body["header"]["title"], "ctx");

    // Over the ceiling is a 400, not a clamp.
    let (st, _) = get_text(
        &client,
        format!("{base}/api/reviews/{id}/context"),
        &[("budget", "999999999")],
    )
    .await;
    assert_eq!(st, 400);
    let (st, _) = get_text(&client, format!("{base}/api/reviews/999999/context"), &[]).await;
    assert_eq!(st, 404);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn other_reviews_disputes_and_waives_on_the_same_paths_are_carried() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let d = repo_tmp.path();
    // A second branch touching the same lib.rb.
    git(d, &["checkout", "-q", "-b", "other", "main"]);
    std::fs::write(d.join("lib.rb"), "def a\n  3\nend\n").unwrap();
    git(d, &["commit", "-aq", "-m", "other"]);
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: std::fs::canonicalize(d).unwrap(),
    }])
    .await;
    let client = reqwest::Client::new();
    let first = create_review(&client, &base, "other", "other review").await;
    let second = create_review(&client, &base, "feature", "this review").await;

    let resp = client
        .post(format!("{base}/api/reviews/{first}/findings/import"))
        .json(&serde_json::json!({
            "schema": "kbc-findings/1",
            "findings": [{
                "slug": "f-old", "severity": "concern", "category": "style",
                "location": {"path": "lib.rb", "kind": "single", "lines": [2]},
                "title": "old complaint", "rationale": "r",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let resp = client
        .put(format!(
            "{base}/api/reviews/{first}/findings/f-old/disposition"
        ))
        .json(&serde_json::json!({ "disposition": "dispute", "note": "not a bug" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let (st, text) = get_text(
        &client,
        format!("{base}/api/reviews/{second}/context"),
        &[("budget", "50000")],
    )
    .await;
    assert_eq!(st, 200, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    let others = body["other_reviews"].as_array().unwrap();
    assert_eq!(others.len(), 1, "{others:?}");
    assert_eq!(others[0]["review_id"], first);
    assert_eq!(others[0]["slug"], "f-old");
    assert_eq!(others[0]["disposition"], "dispute");
    assert_eq!(others[0]["note"], "not a bug");
    assert_eq!(others[0]["path"], "lib.rb");

    // The review itself is not its own "other review".
    let (_, text) = get_text(
        &client,
        format!("{base}/api/reviews/{first}/context"),
        &[("budget", "50000")],
    )
    .await;
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["other_reviews"], serde_json::json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn since_appears_once_a_verdict_sits_on_an_earlier_patchset() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let d = repo_tmp.path();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: std::fs::canonicalize(d).unwrap(),
    }])
    .await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "feature", "ctx").await;
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
    // The author pushes a real change; snapshot ps2.
    std::fs::write(d.join("app.rb"), "require_relative 'lib'\nputs a + 1\n").unwrap();
    git(d, &["commit", "-aq", "-m", "ps2"]);
    let resp = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    let (st, text) = get_text(
        &client,
        format!("{base}/api/reviews/{id}/context"),
        &[("budget", "50000")],
    )
    .await;
    assert_eq!(st, 200, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["ps"], 2);
    assert_eq!(body["since"]["applicable"], true, "{}", body["since"]);
    let report = &body["since"]["report"];
    assert_eq!(report["schema"], "kbc-review-since/1");
    assert_eq!(report["from_source"], "verdict");
    assert_eq!(report["rebase_only"], false);
    assert_eq!(report["author_delta"]["new_hunks"], 1);
    assert_eq!(body["header"]["verdict_stale"], true);

    // Asking for the verdict's own patchset: since does not apply.
    let (_, text) = get_text(
        &client,
        format!("{base}/api/reviews/{id}/context"),
        &[("ps", "1"), ("budget", "50000")],
    )
    .await;
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["ps"], 1);
    assert_eq!(body["since"]["applicable"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explain_base_reports_what_was_recorded_and_why_each_patchset_exists() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: std::fs::canonicalize(repo_tmp.path()).unwrap(),
    }])
    .await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "feature", "ctx").await;
    let (st, text) = get_text(
        &client,
        format!("{base}/api/reviews/{id}/explain-base"),
        &[],
    )
    .await;
    assert_eq!(st, 200, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["schema"], "kbc-review-base-explain/1");
    assert_eq!(body["review_id"], id);
    assert!(body["summary"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(body["base"].is_object());
    let chain = body["chain"].as_array().unwrap();
    assert!(!chain.is_empty());
    assert!(
        chain.iter().all(
            |r| ["applied", "missed", "not-reached", "not-built", "unknown"]
                .contains(&r["status"].as_str().unwrap())
        ),
        "{chain:?}"
    );
    // Never more than one applied rung.
    assert!(chain.iter().filter(|r| r["status"] == "applied").count() <= 1);
    let ps = body["patchsets"].as_array().unwrap();
    assert_eq!(ps.len(), 1);
    assert_eq!(ps[0]["ps"], 1);
    assert!(ps[0]["tip_sha"].as_str().unwrap().len() == 40);

    let (st, _) = get_text(
        &client,
        format!("{base}/api/reviews/999999/explain-base"),
        &[],
    )
    .await;
    assert_eq!(st, 404);
}
