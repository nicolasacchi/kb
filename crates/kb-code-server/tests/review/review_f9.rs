//! v0.44 F9 — end-to-end HTTP tests for `GET /api/reviews/{id}/since`
//! (`crate::review_since`) and `GET /api/reviews/agent-queue`
//! (`crate::review_queue`). Boot pattern mirrors `review_inbox_timeline.rs`.
//! The pure derivations have unit and golden coverage in the modules
//! themselves; these prove the route + store + git WIRING end to end.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

const LIB_V0: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\n";

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

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.txt"), LIB_V0).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("lib.txt"), LIB_V0.replace("l2\n", "l2-author\n")).unwrap();
    std::fs::write(dir.join("feat.txt"), "feature file\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature work"]);
    tmp
}

async fn create_review(client: &reqwest::Client, base: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "r", "head_ref": "feature", "base_ref": "main", "title": "f9",
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
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    resp.json::<serde_json::Value>().await.unwrap()["ps_number"]
        .as_i64()
        .unwrap()
}

async fn get(
    client: &reqwest::Client,
    url: String,
    q: &[(&str, &str)],
) -> (u16, serde_json::Value) {
    let resp = client.get(url).query(q).send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(serde_json::Value::Null))
}

fn paths_of(report: &serde_json::Value) -> Vec<(String, u64, u64, u64)> {
    report["paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["path"].as_str().unwrap().to_string(),
                p["carried"].as_u64().unwrap(),
                p["new"].as_u64().unwrap(),
                p["gone"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn since_reads_a_rebase_as_rebase_only_and_an_edit_as_exactly_the_new_hunk() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir: &Path = repo_tmp.path();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: std::fs::canonicalize(dir).unwrap(),
    }])
    .await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base).await;

    // A verdict on ps1 (the human's act, through the ordinary route).
    let resp = client
        .put(format!("{base}/api/reviews/{id}/verdict"))
        .json(&serde_json::json!({ "state": "approve" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());

    // Upstream moves (an edit elsewhere in a file the PR touches, plus a
    // new file); the author rebases and nothing else changes: ps2.
    git(dir, &["checkout", "-q", "main"]);
    std::fs::write(
        dir.join("lib.txt"),
        LIB_V0.replace("l10\n", "l10-upstream\n"),
    )
    .unwrap();
    std::fs::write(dir.join("up.txt"), "upstream file\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "upstream"]);
    git(dir, &["checkout", "-q", "feature"]);
    git(dir, &["rebase", "main"]);
    assert_eq!(snapshot(&client, &base, id).await, 2);

    // The tip-to-tip interdiff DOES list the upstream file: the noise.
    let (st, inter) = get(
        &client,
        format!("{base}/api/reviews/{id}/interdiff"),
        &[("from", "1"), ("to", "2")],
    )
    .await;
    assert_eq!(st, 200, "{inter}");
    let inter_paths: Vec<&str> = inter["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(inter_paths.contains(&"up.txt"), "{inter_paths:?}");

    // `since` does not: a pure rebase, from the verdict (the default).
    let (st, since) = get(&client, format!("{base}/api/reviews/{id}/since"), &[]).await;
    assert_eq!(st, 200, "{since}");
    assert_eq!(since["schema"], "kbc-review-since/1");
    assert_eq!(since["from_source"], "verdict");
    assert_eq!(
        (since["from"]["ps"].as_u64(), since["to"]["ps"].as_u64()),
        (Some(1), Some(2))
    );
    assert_eq!(since["rebase_only"], true, "{since}");
    assert_eq!(since["bases"]["moved"], true);
    assert_eq!(since["author_delta"]["new_hunks"], 0);
    assert_eq!(since["author_delta"]["gone_hunks"], 0);
    assert_eq!(
        paths_of(&since),
        vec![
            ("feat.txt".to_string(), 1, 0, 0),
            ("lib.txt".to_string(), 1, 0, 0)
        ],
        "upstream's up.txt and its lib.txt edit must not appear"
    );

    // One real author edit: ps3. Exactly that hunk is new.
    std::fs::write(
        dir.join("lib.txt"),
        std::fs::read_to_string(dir.join("lib.txt"))
            .unwrap()
            .replace("l6\n", "l6-author\n"),
    )
    .unwrap();
    git(dir, &["commit", "-aq", "-m", "author edit"]);
    assert_eq!(snapshot(&client, &base, id).await, 3);
    let (st, since) = get(
        &client,
        format!("{base}/api/reviews/{id}/since"),
        &[("from", "ps1"), ("to", "latest")],
    )
    .await;
    assert_eq!(st, 200, "{since}");
    assert_eq!(since["from_source"], "ps");
    assert_eq!(since["rebase_only"], false);
    assert_eq!(since["author_delta"]["new_hunks"], 1);
    assert_eq!(since["author_delta"]["paths_changed"], 1);
    assert_eq!(
        paths_of(&since),
        vec![
            ("feat.txt".to_string(), 1, 0, 0),
            ("lib.txt".to_string(), 1, 1, 0)
        ]
    );

    // Same inputs, same bytes (derived per read, nothing stored).
    let (_, again) = get(
        &client,
        format!("{base}/api/reviews/{id}/since"),
        &[("from", "ps1"), ("to", "latest")],
    )
    .await;
    assert_eq!(since, again);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn since_refuses_honestly() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: std::fs::canonicalize(repo_tmp.path()).unwrap(),
    }])
    .await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base).await;
    let url = format!("{base}/api/reviews/{id}/since");

    // No verdict recorded: `from=verdict` has nothing to start from.
    let (st, body) = get(&client, url.clone(), &[]).await;
    assert_eq!(st, 400, "{body}");
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("no verdict"));
    // A patchset that does not exist is a 404, a malformed ref a 400.
    assert_eq!(
        get(&client, url.clone(), &[("from", "ps1"), ("to", "9")])
            .await
            .0,
        404
    );
    assert_eq!(
        get(&client, url.clone(), &[("from", "banana")]).await.0,
        400
    );
    assert_eq!(
        get(
            &client,
            format!("{base}/api/reviews/9999/since"),
            &[("from", "1")]
        )
        .await
        .0,
        404
    );
    // From a patchset to itself: everything carried, nothing changed.
    let (st, same) = get(&client, url, &[("from", "1"), ("to", "1")]).await;
    assert_eq!(st, 200, "{same}");
    assert_eq!(same["rebase_only"], true);
    assert_eq!(same["bases"]["moved"], false);
}

// --- GET /api/reviews/agent-queue --------------------------------------

async fn ask(client: &reqwest::Client, base: &str, id: i64, body: &str, author: &str) -> String {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "", "anchor_kind": "review", "review_id": id,
            "intent": "question", "body": body, "author": author,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn reply(
    client: &reqwest::Client,
    base: &str,
    parent_id: &str,
    path: &str,
    body: &str,
    author: &str,
) {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": path, "body": body, "parent_id": parent_id, "author": author,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
}

async fn queue(client: &reqwest::Client, base: &str) -> Vec<serde_json::Value> {
    let (st, body) = get(
        client,
        format!("{base}/api/reviews/agent-queue"),
        &[("repo", "r")],
    )
    .await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(body["schema"], "kbc-agent-queue/1");
    let rows = body["rows"].as_array().unwrap().clone();
    assert_eq!(body["count"].as_u64().unwrap() as usize, rows.len());
    rows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_queue_lists_unanswered_human_questions_and_disputes_until_the_agent_replies() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base) = boot(vec![RepoEntry {
        name: "r".to_string(),
        path: std::fs::canonicalize(repo_tmp.path()).unwrap(),
    }])
    .await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base).await;

    assert!(
        queue(&client, &base).await.is_empty(),
        "nothing waiting yet"
    );

    // Lane 1: a human question, and an agent-asked one that is NOT the
    // agent's work (the inbox would count it as "unanswered").
    let q = ask(&client, &base, id, "why this?", "you").await;
    ask(&client, &base, id, "my own open question", "claude").await;
    let rows = queue(&client, &base).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["lane"], "question");
    assert_eq!(rows[0]["annotation_id"], q);
    assert_eq!(rows[0]["review_id"], id);
    assert_eq!(rows[0]["from"], "you");
    assert_eq!(rows[0]["next"][0][2], "comments");

    // Lane 2: a disputed finding.
    let resp = client
        .post(format!("{base}/api/reviews/{id}/findings/import"))
        .json(&serde_json::json!({
            "schema": "kbc-findings/1",
            "findings": [{
                "slug": "f-one", "severity": "concern", "category": "style",
                "location": {"path": "lib.txt", "kind": "whole_file"},
                "title": "t", "rationale": "r",
            }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let resp = client
        .put(format!(
            "{base}/api/reviews/{id}/findings/f-one/disposition"
        ))
        .json(&serde_json::json!({ "disposition": "dispute" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let rows = queue(&client, &base).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0]["lane"], "question", "lane order is fixed");
    assert_eq!(rows[1]["lane"], "dispute");
    assert_eq!(rows[1]["finding_slug"], "f-one");
    let finding_ann = rows[1]["annotation_id"].as_str().unwrap().to_string();

    // The agent answers both (under an agent name): the queue empties.
    reply(&client, &base, &q, "", "because x", "claude").await;
    reply(
        &client,
        &base,
        &finding_ann,
        "re-verified, it stands",
        "claude",
    )
    .await;
    assert!(queue(&client, &base).await.is_empty());

    // A human follow-up puts the question back; byte-identical re-reads.
    // (Annotation timestamps are whole seconds and a same-second tie
    // between replies has no defined order, so step past the agent's.)
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    reply(&client, &base, &q, "", "still unclear", "you").await;
    let a = queue(&client, &base).await;
    let b = queue(&client, &base).await;
    assert_eq!(a.len(), 1);
    assert_eq!(a, b);

    // A bad `state` is a 400, an unknown repo a 404.
    let (st, _) = get(
        &client,
        format!("{base}/api/reviews/agent-queue"),
        &[("state", "weird")],
    )
    .await;
    assert_eq!(st, 400);
    let (st, _) = get(
        &client,
        format!("{base}/api/reviews/agent-queue"),
        &[("repo", "nope")],
    )
    .await;
    assert_eq!(st, 404);
}
