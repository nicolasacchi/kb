//! PRR-R4 ("The PR Room," kb v0.39 T2, Phase 4) — end-to-end HTTP tests for
//! `GET /api/reviews/inbox` (`crate::review_inbox`) and `GET
//! /api/reviews/{id}/timeline` (`crate::review_timeline`). Boot pattern
//! mirrors `review_findings.rs`/`review_routes.rs` — each e2e file in this
//! crate duplicates its own small helper set (see `review_routes.rs`'s own
//! doc for why). The pure scoring/derivation/composition logic already has
//! thorough unit coverage in `crate::review_inbox`/`crate::review_timeline`
//! themselves — these tests exist to prove the HTTP+store WIRING is
//! correct end to end, not to re-derive every edge case.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

#[allow(dead_code)]
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

/// Imports every `slugs` entry in ONE `findings/import` call — deliberately
/// NOT a loop of single-slug calls: the route's default `mode="full"`
/// reconciliation soft-supersedes any EXISTING `origin="import"` finding
/// absent from the CURRENT batch (design doc §4.3), so two separate calls
/// would supersede the first slug the moment the second lands.
async fn import_findings(client: &reqwest::Client, base: &str, id: i64, slugs: &[&str]) {
    let findings: Vec<serde_json::Value> = slugs
        .iter()
        .map(|slug| {
            serde_json::json!({
                "slug": slug,
                "severity": "concern",
                "category": "style",
                "location": {"path": "a.rb", "kind": "whole_file"},
                "title": "t",
                "rationale": "r",
            })
        })
        .collect();
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/import"))
        .json(&serde_json::json!({
            "schema": "kbc-findings/1",
            "findings": findings,
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

async fn import_one_finding(client: &reqwest::Client, base: &str, id: i64, slug: &str) {
    import_findings(client, base, id, &[slug]).await;
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
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
}

/// A review-level ("general") question/note annotation — `path=""`,
/// `anchor_kind="review"` (same shape `review_findings.rs`'s own
/// `review_level_question_round_trips...` test uses).
async fn ask(client: &reqwest::Client, base: &str, id: i64, body: &str, author: &str) -> String {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "",
            "anchor_kind": "review",
            "review_id": id,
            "intent": "question",
            "body": body,
            "author": author,
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

async fn reply(client: &reqwest::Client, base: &str, parent_id: &str, body: &str, author: &str) {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "",
            "body": body,
            "parent_id": parent_id,
            "author": author,
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
}

// --- GET /api/reviews/inbox --------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_route_scores_orders_and_is_deterministic_across_repeated_calls() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R1");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: dir,
    }])
    .await;
    let client = reqwest::Client::new();

    // Review A: two unresolved findings + one unanswered question (asker
    // never followed up by anyone else) -> score = 1*2 + 2 = 4.
    let a = create_review(&client, &base, "r").await;
    import_findings(&client, &base, a, &["f-a1", "f-a2"]).await;
    ask(&client, &base, a, "why not?", "you").await;

    // Review B: one finding, dispositioned "agree" (resolved) -> unresolved
    // = 0; one question ANSWERED by someone else -> unanswered = 0.
    let b = create_review(&client, &base, "r").await;
    import_one_finding(&client, &base, b, "f-b1").await;
    set_disposition(&client, &base, b, "f-b1", "agree").await;
    let q = ask(&client, &base, b, "what about x?", "you").await;
    reply(&client, &base, &q, "handled", "claude").await;

    let fetch_once = || {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .get(format!("{base}/api/reviews/inbox"))
                .query(&[("repo", "r")])
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };

    let first = fetch_once().await;
    let rows = first["reviews"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["review_id"], a, "higher score sorts first");
    assert_eq!(rows[0]["unresolved_findings"], 2);
    assert_eq!(rows[0]["unanswered_questions"], 1);
    assert_eq!(rows[1]["review_id"], b);
    assert_eq!(rows[1]["unresolved_findings"], 0);
    assert_eq!(rows[1]["unanswered_questions"], 0);

    let second = fetch_once().await;
    assert_eq!(first, second, "same state -> byte-identical order/content");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_route_scans_every_configured_repo_when_repo_param_is_absent_and_scopes_when_given() {
    let _guard = SERIAL.lock().await;
    let repo_a = fixture_repo("RA");
    let repo_b = fixture_repo("RB");
    let dir_a = std::fs::canonicalize(repo_a.path()).unwrap();
    let dir_b = std::fs::canonicalize(repo_b.path()).unwrap();
    let (_tmp, base) = boot(vec![
        RepoEntry {
            name: "ra".to_string(),
            path: dir_a,
        },
        RepoEntry {
            name: "rb".to_string(),
            path: dir_b,
        },
    ])
    .await;
    let client = reqwest::Client::new();

    let ida = create_review(&client, &base, "ra").await;
    let idb = create_review(&client, &base, "rb").await;

    let all: serde_json::Value = client
        .get(format!("{base}/api/reviews/inbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<i64> = all["reviews"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["review_id"].as_i64().unwrap())
        .collect();
    assert!(ids.contains(&ida) && ids.contains(&idb));

    let scoped: serde_json::Value = client
        .get(format!("{base}/api/reviews/inbox"))
        .query(&[("repo", "ra")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let scoped_rows = scoped["reviews"].as_array().unwrap();
    assert_eq!(scoped_rows.len(), 1);
    assert_eq!(scoped_rows[0]["review_id"], ida);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_route_rejects_an_unknown_state_value() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R2");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: dir,
    }])
    .await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/reviews/inbox"))
        .query(&[("repo", "r"), ("state", "bogus")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --- GET /api/reviews/{id}/timeline -------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeline_route_returns_ascending_events_end_to_end() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R3");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: dir,
    }])
    .await;
    let client = reqwest::Client::new();

    let id = create_review(&client, &base, "r").await;
    import_one_finding(&client, &base, id, "f-1").await;
    set_disposition(&client, &base, id, "f-1", "agree").await;
    ask(&client, &base, id, "a review-level note", "you").await;

    let body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/timeline"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // V73-K3 — the schema is `/2` since the timeline absorbed the PR body,
    // the working-tree comments, the compose chain, the report, claims,
    // GitHub and the hunk↔turn join. The widening is ADDITIVE: every
    // assertion BELOW this line is unchanged, which is the point.
    assert_eq!(body["schema"], "review-timeline/2");
    assert_eq!(body["review_id"], id);
    let events = body["events"].as_array().unwrap();
    assert!(!events.is_empty());

    // Ascending by `at`.
    let ats: Vec<i64> = events.iter().map(|e| e["at"].as_i64().unwrap()).collect();
    let mut sorted = ats.clone();
    sorted.sort_unstable();
    assert_eq!(ats, sorted, "events must be ascending by `at`");

    let kinds: Vec<&str> = events.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"review_created"));
    assert!(kinds.contains(&"patchset"));
    assert!(kinds.contains(&"findings_import"));
    assert!(kinds.contains(&"disposition"));
    assert!(kinds.contains(&"comment"));

    let import_evt = events
        .iter()
        .find(|e| e["kind"] == "findings_import")
        .unwrap();
    assert_eq!(import_evt["count"], 1);
    assert_eq!(import_evt["slugs"], serde_json::json!(["f-1"]));

    let disp_evt = events.iter().find(|e| e["kind"] == "disposition").unwrap();
    assert_eq!(disp_evt["slug"], "f-1");
    assert_eq!(disp_evt["state"], "agree");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeline_route_on_a_brand_new_review_has_only_review_created_and_patchset() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo("R4");
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: dir,
    }])
    .await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r").await;

    let body: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/timeline"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let kinds: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["review_created", "patchset"]);
}
