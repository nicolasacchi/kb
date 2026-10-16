//! OL S1 — end-to-end HTTP tests for the kb relays (`GET /api/kb/search`,
//! `GET /api/kb/ui`) against a real daemon booted over a mock kb.
//!
//! Pins: the permalink is built on the PUBLIC base (not the federation url);
//! a short query never reaches kb; every degraded state — kb unreachable,
//! `[kb_daemon]` disabled, kb-sibling/1 mismatch — is a 200 with an
//! `unavailable` caption (never a 5xx); the relays carry no CORS headers.

use axum::{extract::Query, routing::get, Json, Router};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::common::{git, init_repo};

async fn spawn_mock(identity: Value, search_calls: Arc<AtomicUsize>) -> SocketAddr {
    let router = Router::new()
        .route(
            "/api/identity",
            get(move || {
                let identity = identity.clone();
                async move { Json(identity) }
            }),
        )
        .route(
            "/api/search",
            get(move |Query(p): Query<HashMap<String, String>>| {
                let calls = search_calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Json(json!({
                        "hits": [{
                            "kb": "platform",
                            "id": "abc123",
                            "title": format!("hit for {}", p.get("q").cloned().unwrap_or_default()),
                            "source_relative": "design/theme notes.html",
                            "snippet": "a window of body text",
                        }],
                        "ms": 1
                    }))
                }
            }),
        )
        .route(
            "/api/settings",
            get(|| async {
                Json(json!({
                    "theme": "dark",
                    "accent": null,
                    "density": "comfy",
                    "theme_family": "catppuccin"
                }))
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

async fn boot(kb: KbDaemonSection) -> Boot {
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

fn hello_ok() -> Value {
    json!({
        "name": "kb",
        "sibling_protocol": kb_core::sibling::SIBLING_PROTOCOL,
        "sibling_major": kb_core::sibling::SIBLING_MAJOR
    })
}

async fn get_json(
    base: &str,
    path: &str,
) -> (reqwest::StatusCode, reqwest::header::HeaderMap, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    (status, headers, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_relays_hits_with_public_permalinks_and_never_cors() {
    let calls = Arc::new(AtomicUsize::new(0));
    let kb = spawn_mock(hello_ok(), calls.clone()).await;
    let b = boot(KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{kb}")),
        token_file: None,
        public_url: Some("https://kb.example.com".to_string()),
    })
    .await;

    let (status, headers, body) =
        get_json(&b.base, "/api/kb/search?q=theme%20family&limit=5").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["kb_public_url"], "https://kb.example.com");
    assert_eq!(body["hits"][0]["title"], "hit for theme family");
    assert_eq!(
        body["hits"][0]["url"], "https://kb.example.com/a/platform/design/theme%20notes.html",
        "permalink must use the PUBLIC base, not the federation url"
    );
    assert!(body.get("unavailable").is_none());
    assert!(
        headers.get("access-control-allow-origin").is_none(),
        "the relay must never join the CORS route set"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_one_char_query_never_reaches_kb() {
    let calls = Arc::new(AtomicUsize::new(0));
    let kb = spawn_mock(hello_ok(), calls.clone()).await;
    let b = boot(KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{kb}")),
        token_file: None,
        public_url: Some("https://kb.example.com".to_string()),
    })
    .await;

    for path in [
        "/api/kb/search?q=a",
        "/api/kb/search?q=%20%20",
        "/api/kb/search",
    ] {
        let (status, _h, body) = get_json(&b.base, path).await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(body["hits"], json!([]), "{path}");
        assert!(body.get("unavailable").is_none(), "{path}");
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_disabled_and_mismatched_kb_degrade_to_a_200_caption() {
    // Unreachable: nothing listens on the port.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let b = boot(KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{dead}")),
        token_file: None,
        public_url: Some("https://kb.example.com".to_string()),
    })
    .await;
    let (status, _h, body) = get_json(&b.base, "/api/kb/search?q=theme").await;
    assert_eq!(status, 200);
    assert_eq!(body["hits"], json!([]));
    assert_eq!(body["unavailable"], "kb unreachable");
    let (status, _h, ui) = get_json(&b.base, "/api/kb/ui").await;
    assert_eq!(status, 200);
    assert!(ui["theme_family"].is_null());
    assert_eq!(ui["unavailable"], "kb unreachable");

    // Disabled.
    let b = boot(KbDaemonSection {
        enabled: false,
        url: None,
        token_file: None,
        public_url: None,
    })
    .await;
    let (status, _h, body) = get_json(&b.base, "/api/kb/search?q=theme").await;
    assert_eq!(status, 200);
    assert_eq!(body["unavailable"], "kb disabled");

    // kb-sibling/1 mismatch: fails closed, distinct caption.
    let kb = spawn_mock(
        json!({"sibling_protocol": "kb-sibling/9", "sibling_major": 9}),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let b = boot(KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{kb}")),
        token_file: None,
        public_url: Some("https://kb.example.com".to_string()),
    })
    .await;
    let (status, _h, body) = get_json(&b.base, "/api/kb/search?q=theme").await;
    assert_eq!(status, 200);
    assert_eq!(body["hits"], json!([]));
    assert_eq!(body["unavailable"], "kb_sibling_mismatch");
    let (status, _h, ui) = get_json(&b.base, "/api/kb/ui").await;
    assert_eq!(status, 200);
    assert_eq!(ui["unavailable"], "kb_sibling_mismatch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ui_relays_kbs_theme_family_read_only() {
    let kb = spawn_mock(hello_ok(), Arc::new(AtomicUsize::new(0))).await;
    let b = boot(KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{kb}")),
        token_file: None,
        public_url: None,
    })
    .await;
    let (status, headers, body) = get_json(&b.base, "/api/kb/ui").await;
    assert_eq!(status, 200);
    assert_eq!(body["theme_family"], "catppuccin");
    assert_eq!(body["theme"], "dark");
    assert!(body.get("unavailable").is_none());
    assert!(headers.get("access-control-allow-origin").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enabled_kb_without_a_public_url_never_leaks_the_internal_url() {
    let calls = Arc::new(AtomicUsize::new(0));
    let kb = spawn_mock(hello_ok(), calls.clone()).await;
    let b = boot(KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{kb}")),
        token_file: None,
        public_url: None,
    })
    .await;
    let (status, _h, body) = get_json(&b.base, "/api/kb/search?q=theme").await;
    assert_eq!(status, 200);
    assert_eq!(body["hits"], json!([]));
    assert_eq!(body["unavailable"], "kb public url not configured");
    assert_eq!(body["kb_public_url"], "");
    assert!(!body.to_string().contains(&kb.to_string()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_overlong_query_is_truncated_before_it_reaches_kb() {
    let calls = Arc::new(AtomicUsize::new(0));
    let kb = spawn_mock(hello_ok(), calls.clone()).await;
    let b = boot(KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{kb}")),
        token_file: None,
        public_url: Some("https://kb.example.com".to_string()),
    })
    .await;
    let q = "é".repeat(1000);
    let (status, _h, body) = get_json(&b.base, &format!("/api/kb/search?q={q}")).await;
    assert_eq!(status, 200);
    // the mock echoes the q it received in the hit title
    let title = body["hits"][0]["title"].as_str().unwrap();
    assert_eq!(title, format!("hit for {}", "é".repeat(256)));
}
