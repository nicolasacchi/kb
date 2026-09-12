//! SEC-20 — the append-only mutations ledger (V0027) and `GET /api/audit`.
//!
//! What these pin:
//!
//! * one row per mutating request, and NONE for a read (a ledger that
//!   logged reads would drown the thing it exists to make findable);
//! * the FAILED attempt is in the ledger too (its `outcome` is the status
//!   code) — an attempt is evidence;
//! * the admission rung is DERIVED from the loopback predicate, not from
//!   anything the caller can set;
//! * `?since=`/`?limit=` window and cap, newest first.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;

use crate::common::git;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

async fn boot(path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fx".to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

async fn audit(base: &str, query: &[(&str, &str)]) -> serde_json::Value {
    reqwest::Client::new()
        .get(format!("{base}/api/audit"))
        .query(query)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn a_mutation_is_recorded_and_a_read_is_not() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path()).await;
    let client = reqwest::Client::new();

    // A read...
    client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap();
    let body = audit(&base, &[]).await;
    assert_eq!(body["schema"].as_str(), Some("kbc-audit/1"));
    assert_eq!(body["count"].as_i64(), Some(0), "reads are not mutations");

    // ...then a mutation.
    let resp = client
        .post(format!("{base}/api/backfill?repo=fx"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let body = audit(&base, &[]).await;
    assert_eq!(body["count"].as_i64(), Some(1), "body: {body}");
    let e = &body["entries"][0];
    // The FULL path a caller can address — not the nest-stripped
    // `/backfill` (`security::full_path`'s doc explains why that
    // distinction is load-bearing rather than cosmetic).
    assert_eq!(e["route"].as_str(), Some("/api/backfill"));
    assert_eq!(e["method"].as_str(), Some("POST"));
    assert_eq!(e["admission"].as_str(), Some("loopback"));
    assert_eq!(e["repo"].as_str(), Some("fx"));
    assert_eq!(e["outcome"].as_str(), Some("200"));
    assert_eq!(e["request_id"].as_str().unwrap().len(), 12);
    assert!(e["ts"].as_str().unwrap().contains('T'));
}

#[tokio::test]
async fn a_failed_mutation_is_recorded_with_its_status() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path()).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/backfill?repo=nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "unknown repo");

    let body = audit(&base, &[]).await;
    assert_eq!(body["count"].as_i64(), Some(1));
    assert_eq!(body["entries"][0]["outcome"].as_str(), Some("404"));
    assert_eq!(body["entries"][0]["repo"].as_str(), Some("nope"));
}

#[tokio::test]
async fn a_refused_request_is_not_a_mutation_that_happened() {
    // The audit middleware runs INSIDE the origin guards, so a request the
    // allowlist refused never reached a handler and is not in the ledger.
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path()).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/backfill?repo=fx"))
        .header("Host", "attacker.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    assert_eq!(audit(&base, &[]).await["count"].as_i64(), Some(0));
}

#[tokio::test]
async fn the_window_and_the_cap_are_enforced_and_rows_are_newest_first() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(repo.path()).await;
    let client = reqwest::Client::new();
    for _ in 0..3 {
        client
            .post(format!("{base}/api/backfill?repo=fx"))
            .send()
            .await
            .unwrap();
    }

    let all = audit(&base, &[]).await;
    assert_eq!(all["count"].as_i64(), Some(3));
    // Newest first: ids strictly decrease.
    let ids: Vec<i64> = all["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap())
        .collect();
    assert!(ids.windows(2).all(|w| w[0] > w[1]), "{ids:?}");

    let capped = audit(&base, &[("limit", "2")]).await;
    assert_eq!(capped["count"].as_i64(), Some(2));
    assert_eq!(capped["limit"].as_i64(), Some(2));

    // A `since` in the future selects nothing.
    let future = (chrono::Utc::now().timestamp() + 3600).to_string();
    let none = audit(&base, &[("since", &future)]).await;
    assert_eq!(none["count"].as_i64(), Some(0));

    // The server caps `limit` at 500 regardless of what a caller asks for.
    let over = audit(&base, &[("limit", "100000")]).await;
    assert_eq!(over["limit"].as_i64(), Some(500));
}
