//! OL PRES2 - end-to-end HTTP tests for `GET /api/kb/presence` against a real
//! daemon booted over a mock kb that serves the COMMITTED live-touches
//! fixture (`tests/fixtures/live-touches-v1.json`, the PRES1 contract).
//!
//! The web-code e2e never starts kb, so this is the one place the real route,
//! the real `KbClient` handshake and kb's wire shape meet. Pins: the repo
//! alias lands a hook project label in its repo; a worktree touch is its own
//! row; an unmapped label is captioned; the scoped request sends the repo's
//! labels to kb as `project=` filters; every degraded state is a 200 with a
//! caption, never a 5xx.

use axum::{routing::get, Json, Router};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::common::{git, init_repo};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/live-touches-v1.json");

fn hello_ok() -> Value {
    json!({
        "name": "kb",
        "sibling_protocol": kb_core::sibling::SIBLING_PROTOCOL,
        "sibling_major": kb_core::sibling::SIBLING_MAJOR
    })
}

async fn spawn_mock(identity: Value, queries: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let router = Router::new()
        .route(
            "/api/identity",
            get(move || {
                let identity = identity.clone();
                async move { Json(identity) }
            }),
        )
        .route(
            "/api/sessions/live-touches",
            get(move |uri: axum::http::Uri| {
                let queries = queries.clone();
                async move {
                    queries
                        .lock()
                        .unwrap()
                        .push(uri.query().unwrap_or("").to_string());
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        FIXTURE,
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

struct Boot {
    _tmp: tempfile::TempDir,
    _repo: tempfile::TempDir,
    base: String,
    _task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(kb: KbDaemonSection, aliases: &[&str]) -> Boot {
    let repo = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"x\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir,
            projects: aliases.iter().map(|s| s.to_string()).collect(),
        }],
        kb_daemon: kb,
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    Boot {
        _tmp: tmp,
        _repo: repo,
        base: format!("http://{addr}"),
        _task: task,
    }
}

fn enabled(kb: SocketAddr) -> KbDaemonSection {
    KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{kb}")),
        token_file: None,
        public_url: None,
    }
}

async fn get_json(base: &str, path: &str) -> (reqwest::StatusCode, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fixture_flows_through_the_real_route_with_an_alias_and_a_worktree_row() {
    let queries = Arc::new(Mutex::new(Vec::new()));
    let kb = spawn_mock(hello_ok(), queries.clone()).await;
    let b = boot(enabled(kb), &["demo"]).await;

    let (status, body) = get_json(&b.base, "/api/kb/presence?repo=fixture").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["schema"], "kbc-presence/1");
    assert_eq!(body["available"], true);
    assert!(body.get("caption").is_none(), "{body}");
    let files = body["files"].as_array().unwrap();
    // Four touches, four distinct (worktree, path, session) rows.
    assert_eq!(files.len(), 4, "{body}");
    assert!(files.iter().all(|f| f["repo"] == "fixture"));
    // Newest first.
    let ages: Vec<i64> = files
        .iter()
        .map(|f| f["age_secs"].as_i64().unwrap())
        .collect();
    assert_eq!(ages, [4, 30, 60, 120]);
    // The worktree touch is its own row and carries the basename; the main
    // checkout rows carry no worktree key at all.
    let wt: Vec<&Value> = files
        .iter()
        .filter(|f| f.get("worktree").is_some())
        .collect();
    assert_eq!(wt.len(), 1);
    assert_eq!(wt[0]["worktree"], "demo-feature");
    assert_eq!(wt[0]["path"], "src/lib.rs");
    assert!(files[0].get("worktree").is_none());
    // No absolute path anywhere in the payload.
    assert!(!body.to_string().contains("/home/"));
    // The scoped call sent the repo's labels (alias first, then name) as filters.
    let q = queries.lock().unwrap().last().cloned().unwrap();
    assert!(q.contains("project=demo"), "{q}");
    assert!(q.contains("project=fixture"), "{q}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_alias_the_same_touches_are_captioned_outside_configured_repos() {
    let queries = Arc::new(Mutex::new(Vec::new()));
    let kb = spawn_mock(hello_ok(), queries).await;
    let b = boot(enabled(kb), &[]).await;

    let (status, body) = get_json(&b.base, "/api/kb/presence").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["available"], true);
    assert_eq!(body["files"].as_array().unwrap().len(), 0);
    assert_eq!(body["caption"], "4 edits outside configured repos");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn degraded_states_are_200_with_a_caption_never_a_5xx() {
    // Unknown repo.
    let kb = spawn_mock(hello_ok(), Arc::new(Mutex::new(Vec::new()))).await;
    let b = boot(enabled(kb), &["demo"]).await;
    let (status, body) = get_json(&b.base, "/api/kb/presence?repo=nope").await;
    assert_eq!(status, 200);
    assert_eq!(body["available"], false);
    assert_eq!(body["caption"], "unknown repo");

    // Sibling mismatch.
    let kb = spawn_mock(
        json!({"name": "kb", "sibling_protocol": "kb-sibling/99", "sibling_major": 99}),
        Arc::new(Mutex::new(Vec::new())),
    )
    .await;
    let b = boot(enabled(kb), &["demo"]).await;
    let (status, body) = get_json(&b.base, "/api/kb/presence").await;
    assert_eq!(status, 200);
    assert_eq!(body["available"], false);
    assert_eq!(body["caption"], "kb_sibling_mismatch");

    // Disabled.
    let b = boot(
        KbDaemonSection {
            enabled: false,
            url: None,
            token_file: None,
            public_url: None,
        },
        &[],
    )
    .await;
    let (status, body) = get_json(&b.base, "/api/kb/presence").await;
    assert_eq!(status, 200);
    assert_eq!(body["available"], false);
    assert_eq!(body["caption"], "kb disabled");

    // Unreachable.
    let b = boot(
        KbDaemonSection {
            enabled: true,
            url: Some("http://127.0.0.1:1".to_string()),
            token_file: None,
            public_url: None,
        },
        &[],
    )
    .await;
    let (status, body) = get_json(&b.base, "/api/kb/presence").await;
    assert_eq!(status, 200);
    assert_eq!(body["available"], false);
    assert_eq!(body["caption"], "kb unreachable");
}
