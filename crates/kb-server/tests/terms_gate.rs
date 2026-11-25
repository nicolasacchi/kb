//! The SPA-only terminal gate over the REAL router, with a FAKE unix
//! runner: ten routes, one scripted daemon.
//!
//! The runner is a `tokio::net::UnixListener` task in the test's
//! tempdir; `runner_socket` points at it. It counts connections and
//! returns scripted replies, so "the gate never touched the runner" is
//! an observable assertion, not an assumption.

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, TerminalSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

const FIXTURE_TOKEN: &str = "test-token-fixture-deadbeef";
const TERM_ID: &str = "abcd1234";

fn kb_section(path: std::path::PathBuf) -> KbSection {
    KbSection {
        path,
        skip_patterns: Vec::new(),
        ui: UiSection::default(),
        embedding_model: None,
        reranker_model: None,
        chunked_embeddings: false,
        graph_boost: None,
        outbound: None,
        atlas: None,
        templates: std::collections::BTreeMap::new(),
        memory_scope: None,
        project_slugs: Vec::new(),
        default_search_category: None,
        code_url: None,
        decay_policy: None,
        versions: None,
        git_dir: None,
        git_prefix: None,
        reading_progress: None,
        search: Default::default(),
        indexable_extensions: None,
        reconcile_secs: None,
        capture_dir: None,
        resurface: None,
        slo: None,
        id_patterns: Vec::new(),
    }
}

/// Scripted replies keyed by op, plus a connection counter to prove
/// "the gate never touched the runner", plus the raw request lines so
/// relay payloads can be asserted per test.
struct FakeRunner {
    conns: Arc<AtomicUsize>,
    reqs: Arc<Mutex<Vec<serde_json::Value>>>,
    socket: std::path::PathBuf,
}

fn spawn_fake(
    dir: &std::path::Path,
    replies: std::collections::HashMap<String, String>,
) -> FakeRunner {
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let reqs: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let counter = conns.clone();
    let sink = reqs.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let replies = replies.clone();
            let sink = sink.clone();
            tokio::spawn(async move {
                let mut line = String::new();
                let mut reader = tokio::io::BufReader::new(stream);
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let parsed: serde_json::Value =
                    serde_json::from_str(line.trim()).unwrap_or_default();
                let op = parsed
                    .get("op")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_default();
                sink.lock().unwrap().push(parsed);
                let reply = replies.get(&op).cloned().unwrap_or_else(|| {
                    "{\"status\":\"err\",\"code\":\"not-found\",\"message\":\"no script\"}"
                        .to_string()
                });
                let mut stream = reader.into_inner();
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(b"\n").await;
            });
        }
    });
    FakeRunner {
        conns,
        reqs,
        socket: sock,
    }
}

fn list_reply(ids: &[&str]) -> String {
    let items: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "harness": "omp",
                "cwd": "/work/proj",
                "started_unix": 1234567890,
                "resume_sid": null,
                "pane_pid": 4242,
                "cols": 120,
                "rows": 40,
                "bracket_paste_flag": true,
                "pane_dead": false,
                "exit_code": null,
                "grid_owner": null,
            })
        })
        .collect();
    serde_json::json!({"status":"ok","data":items}).to_string()
}

fn ok_reply(data: &str) -> String {
    format!("{{\"status\":\"ok\",\"data\":{data}}}")
}

async fn boot(
    terminal: TerminalSection,
    fake: &FakeRunner,
    with_token: bool,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    boot_with(terminal, fake, with_token).await
}

async fn boot_with(
    terminal: TerminalSection,
    fake: &FakeRunner,
    with_token: bool,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source));
    let mut terminal = terminal;
    terminal.runner_socket = Some(fake.socket.clone());
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        ui: UiSection::default(),
        kb: kb_map,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        terminal,
        ..Default::default()
    };

    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    if with_token {
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    }
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    (tmp, addr)
}

fn enabled_terminal() -> TerminalSection {
    TerminalSection {
        enabled: Some(true),
        ..Default::default()
    }
}

fn base_url(addr: std::net::SocketAddr) -> String {
    format!("http://{addr}")
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn origin_header(addr: std::net::SocketAddr) -> String {
    format!("http://{addr}")
}

async fn body_text(resp: reqwest::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    (status, text)
}

fn assert_problem(text: &str, urn_tail: &str) {
    assert!(
        text.contains(urn_tail),
        "response must carry {urn_tail}, got: {text}"
    );
}

#[tokio::test]
async fn disabled_by_default_returns_bodiless_404_on_every_terms_route() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(TerminalSection::default(), &fake, false).await;
    let c = client();
    let origin = origin_header(addr);
    // All fourteen routes with valid Origin+intent → 404 empty body, no
    // content-type, fake runner saw 0 connections.
    let routes: Vec<(&str, &str, Option<&str>)> = vec![
        ("GET", "/api/terms", None),
        ("POST", "/api/terms", Some("{}")),
        ("POST", "/api/terms/abcd1234/ticket", Some("{}")),
        (
            "POST",
            "/api/terms/abcd1234/paste",
            Some("{\"text\":\"hi\"}"),
        ),
        ("POST", "/api/terms/abcd1234/enter", Some("{}")),
        (
            "POST",
            "/api/terms/abcd1234/keys",
            Some("{\"keys\":[\"Enter\"]}"),
        ),
        ("POST", "/api/terms/abcd1234/kill", None),
        (
            "POST",
            "/api/terms/abcd1234/take-size",
            Some("{\"cols\":80,\"rows\":24}"),
        ),
        ("GET", "/api/terms/abcd1234/read", None),
        ("GET", "/api/terms/abcd1234/history", None),
        ("GET", "/api/terms/abcd1234/grid", None),
        ("POST", "/api/terms/abcd1234/release-size", Some("{}")),
        (
            "POST",
            "/api/terms/abcd1234/resize",
            Some("{\"cols\":80,\"rows\":24}"),
        ),
        ("POST", "/api/terms/abcd1234/viewer-close", Some("{}")),
    ];
    for (m, path, body) in routes {
        let mut req = match m {
            "GET" => c.get(format!("{}{}", base_url(addr), path)),
            _ => c.post(format!("{}{}", base_url(addr), path)),
        }
        .header("Origin", origin.clone())
        .header("x-kb-intent", "terminal");
        if let Some(b) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(b.to_string());
        }
        let resp = req.send().await.expect("request");
        assert_eq!(resp.status().as_u16(), 404, "{m} {path}");
        assert!(
            resp.headers().get("content-type").is_none(),
            "{m} {path} must be bodiless (no content-type)"
        );
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store"),
            "{m} {path} must carry Cache-Control: no-store"
        );
        let (status, text) = body_text(resp).await;
        assert_eq!(status, 404);
        assert!(text.is_empty(), "{m} {path} must be bodiless, got: {text}");
    }
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "a disabled daemon must never dial the runner"
    );
}

/// A same-origin browser `fetch()` GET carries no `Origin`. With the
/// intent header it is admitted on every GET route (FUTERMGET); without
/// it, it is refused with the intent refusal and never dials the runner.
#[tokio::test]
async fn absent_origin_with_intent_is_admitted_on_get() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    replies.insert("read".to_string(), ok_reply("{\"text\":\"\"}"));
    replies.insert("history".to_string(), ok_reply("{\"text\":\"\"}"));
    replies.insert(
        "grid".to_string(),
        ok_reply("{\"cols\":80,\"rows\":24,\"owner\":null}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for suffix in ["", "/abcd1234/read", "/abcd1234/history", "/abcd1234/grid"] {
        let resp = c
            .get(format!("{}/api/terms{suffix}", base_url(addr)))
            .header("x-kb-intent", "terminal")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200, "GET {suffix}, no Origin");
    }
    let before = fake.conns.load(Ordering::SeqCst);
    assert_eq!(before, 4, "each admitted GET dialled the runner once");
    let resp = c
        .get(format!("{}/api/terms", base_url(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-intent-required");
    assert_eq!(fake.conns.load(Ordering::SeqCst), before);
}

/// The GET gate table (FUTERMGET), for every GET route: what a browser's
/// same-origin read sends is admitted; every other shape is refused
/// before the runner is dialled.
#[tokio::test]
async fn get_gate_table_over_every_get_route() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    replies.insert("read".to_string(), ok_reply("{\"text\":\"\"}"));
    replies.insert("history".to_string(), ok_reply("{\"text\":\"\"}"));
    replies.insert(
        "grid".to_string(),
        ok_reply("{\"cols\":80,\"rows\":24,\"owner\":null}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, true).await;
    let c = client();
    let own = origin_header(addr);
    let get = |suffix: &str| c.get(format!("{}/api/terms{suffix}", base_url(addr)));
    for suffix in ["", "/abcd1234/read", "/abcd1234/history", "/abcd1234/grid"] {
        // Allowed: absent Origin + intent + same-origin / none / no fetch metadata.
        for site in [Some("same-origin"), Some("none"), None] {
            let mut req = get(suffix).header("x-kb-intent", "terminal");
            if let Some(v) = site {
                req = req.header("sec-fetch-site", v);
            }
            let resp = req.send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 200, "{suffix} site {site:?}");
        }
        // Allowed: the exact own Origin too.
        let resp = get(suffix)
            .header("x-kb-intent", "terminal")
            .header("Origin", own.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200, "{suffix} own origin");
    }
    let admitted = fake.conns.load(Ordering::SeqCst);
    assert_eq!(admitted, 16);

    for suffix in ["", "/abcd1234/read", "/abcd1234/history", "/abcd1234/grid"] {
        // Refused: no intent (with and without an Origin).
        for origin in [None, Some(own.clone())] {
            let mut req = get(suffix);
            if let Some(o) = origin {
                req = req.header("Origin", o);
            }
            let resp = req.send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 403, "{suffix} no intent");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-intent-required");
        }
        // Refused: cross-site and same-site fetch metadata.
        for site in ["cross-site", "same-site"] {
            let resp = get(suffix)
                .header("x-kb-intent", "terminal")
                .header("sec-fetch-site", site)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 403, "{suffix} site {site}");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-origin-refused");
        }
        // Refused: a present wrong Origin (an artifact subdomain, a foreign host).
        for origin in ["http://x.artifacts.localhost", "https://evil.example"] {
            let resp = get(suffix)
                .header("x-kb-intent", "terminal")
                .header("Origin", origin)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 403, "{suffix} origin {origin}");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-origin-refused");
        }
        // Refused: a token caller (the default refuses them).
        let resp = get(suffix)
            .header("x-kb-intent", "terminal")
            .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{suffix} token caller");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-token-refused");
    }
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        admitted,
        "no refused GET may dial the runner"
    );

    // The POST gate is unchanged: an absent Origin still fails closed.
    let resp = c
        .post(format!("{}/api/terms/abcd1234/kill", base_url(addr)))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-origin-refused");
}

#[tokio::test]
async fn artifact_subdomain_and_foreign_origins_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for origin in [
        "https://evil.example",
        "http://x.artifacts.localhost",
        "http://localhost:9999",
    ] {
        let resp = c
            .get(format!("{}/api/terms", base_url(addr)))
            .header("Origin", origin)
            .header("x-kb-intent", "terminal")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "origin {origin}");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-origin-refused");
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn same_origin_host_is_accepted() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    // reqwest sends Origin exactly equal to the Host it dials.
    let resp = c
        .get(format!("{}/api/terms", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let (_s, text) = body_text(resp).await;
    assert!(text.contains(TERM_ID), "must relay the list: {text}");
}

#[tokio::test]
async fn post_without_intent_header_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for intent in [None, Some("nope")] {
        let mut req = c
            .post(format!("{}/api/terms/abcd1234/kill", base_url(addr)))
            .header("Origin", origin_header(addr));
        if let Some(v) = intent {
            req = req.header("x-kb-intent", v);
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "intent {intent:?}");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-intent-required");
    }
    // `Terminal` (case-insensitive) passes the gate — the kill is
    // relayed to the fake (which answers not_found → 404).
    let resp = c
        .post(format!("{}/api/terms/abcd1234/kill", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "Terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        1,
        "Terminal must reach the runner"
    );
}

/// Every mutating terms route must demand `X-Kb-Intent: terminal` and an
/// Origin; every GET must demand the intent header and refuse a wrong
/// Origin. A handler that skipped its gate would let one through and open
/// the runner.
#[tokio::test]
async fn every_terms_route_enforces_intent_and_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let posts: Vec<(&str, &str)> = vec![
        ("", "{\"harness\":\"omp\",\"cwd\":\"/work/proj\"}"),
        ("/abcd1234/ticket", "{}"),
        ("/abcd1234/paste", "{\"text\":\"hi\"}"),
        ("/abcd1234/enter", "{}"),
        ("/abcd1234/keys", "{\"keys\":[\"Enter\"]}"),
        ("/abcd1234/kill", "{}"),
        ("/abcd1234/take-size", "{\"cols\":80,\"rows\":24}"),
        ("/abcd1234/release-size", "{}"),
        ("/abcd1234/resize", "{\"cols\":80,\"rows\":24}"),
        ("/abcd1234/viewer-close", "{}"),
    ];
    for (suffix, body) in &posts {
        for intent in [None, Some("nope")] {
            let mut req = c
                .post(format!("{}/api/terms{suffix}", base_url(addr)))
                .header("Origin", origin_header(addr))
                .header("Content-Type", "application/json")
                .body(body.to_string());
            if let Some(v) = intent {
                req = req.header("x-kb-intent", v);
            }
            let resp = req.send().await.unwrap();
            assert_eq!(
                resp.status().as_u16(),
                403,
                "POST {suffix} intent {intent:?}"
            );
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-intent-required");
        }
        // No Origin at all, even with the intent header: refused.
        let resp = c
            .post(format!("{}/api/terms{suffix}", base_url(addr)))
            .header("x-kb-intent", "terminal")
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "POST {suffix} without Origin");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-origin-refused");
    }
    for suffix in ["", "/abcd1234/read", "/abcd1234/history", "/abcd1234/grid"] {
        // GETs: no intent is refused (a same-origin read sends the
        // intent header but no Origin; see `get_gate_table_over_every_get_route`).
        let resp = c
            .get(format!("{}/api/terms{suffix}", base_url(addr)))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "GET {suffix} without intent");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-intent-required");
        // A present wrong Origin is refused even with the intent header.
        let resp = c
            .get(format!("{}/api/terms{suffix}", base_url(addr)))
            .header("x-kb-intent", "terminal")
            .header("Origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "GET {suffix} wrong Origin");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-origin-refused");
    }
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "a refused request must never dial the runner"
    );
}

#[tokio::test]
async fn get_routes_need_the_intent_header() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = c
        .get(format!("{}/api/terms", base_url(addr)))
        .header("Origin", origin_header(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-intent-required");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn loopback_caller_without_origin_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    // A loopback peer (this process) sending nothing: the auth
    // bypass admits it, but the gate refuses it — no bypass.
    let c = client();
    let resp = c
        .post(format!("{}/api/terms/abcd1234/kill", base_url(addr)))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-origin-refused");
}

#[tokio::test]
async fn legacy_token_callers_are_refused_by_default_and_allowed_when_configured() {
    // Default: refuse.
    {
        let tmp = tempfile::tempdir().unwrap();
        let fake = spawn_fake(tmp.path(), Default::default());
        let (_tmp, addr) = boot(enabled_terminal(), &fake, true).await;
        let c = client();
        let resp = c
            .post(format!("{}/api/terms/abcd1234/kill", base_url(addr)))
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403);
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-token-refused");
        assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
    }
    // Configured off: the gate passes (the runner answers not_found).
    {
        let tmp = tempfile::tempdir().unwrap();
        let fake = spawn_fake(tmp.path(), Default::default());
        let t = TerminalSection {
            refuse_token_callers: Some(false),
            ..enabled_terminal()
        };
        let (_tmp, addr) = boot(t, &fake, true).await;
        let c = client();
        let resp = c
            .post(format!("{}/api/terms/abcd1234/kill", base_url(addr)))
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);
        assert_eq!(fake.conns.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn ticket_mint_returns_hex_and_is_unique_per_call() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let mut tickets = Vec::new();
    for _ in 0..2 {
        let resp = c
            .post(format!("{}/api/terms/{TERM_ID}/ticket", base_url(addr)))
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        let (_s, text) = body_text(resp).await;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let ticket = v["ticket"].as_str().unwrap().to_string();
        assert_eq!(ticket.len(), 32);
        assert!(ticket
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()));
        assert_eq!(v["expires_in"], 30);
        assert_eq!(v["term"], TERM_ID);
        tickets.push(ticket);
    }
    assert_ne!(tickets[0], tickets[1], "every mint must be distinct");
}

#[tokio::test]
async fn ticket_is_not_minted_for_an_unknown_terminal() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), list_reply(&["efgh5678"]));
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/ticket", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
}

#[tokio::test]
async fn terminal_id_charset_is_enforced_before_the_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    // Over the 32-char unified grammar, so a runner connection must never open.
    for id in ["..%2Fx", "A1", "a".repeat(33).as_str(), "a%20b"] {
        let resp = c
            .post(format!("{}/api/terms/{id}/kill", base_url(addr)))
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400, "id {id}");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "bad-request");
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0, "0 runner connections");
}

#[tokio::test]
async fn launch_rejects_unknown_harness_and_bad_cwd() {
    let mut replies = std::collections::HashMap::new();
    replies.insert(
        "launch".to_string(),
        ok_reply("{\"id\":\"abcd1234\",\"harness\":\"omp\"}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let launch = |body: String| {
        c.post(format!("{}/api/terms", base_url(addr)))
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .header("Content-Type", "application/json")
            .body(body)
    };
    for body in [
        "{\"harness\":\"emacs\",\"cwd\":\"/work/proj\"}".to_string(),
        // kimi support was removed: a launch naming it is an unknown harness.
        "{\"harness\":\"kimi\",\"cwd\":\"/work/proj\"}".to_string(),
        "{\"harness\":\"omp\",\"cwd\":\"rel/path\"}".to_string(),
        "{\"harness\":\"omp\",\"cwd\":\"/work/proj\",\"resume_sid\":\"bad\\u0000sid\"}".to_string(),
        "{\"harness\":\"omp\",\"cwd\":\"/work/proj\",\"unknown\":1}".to_string(),
    ] {
        let resp = launch(body.clone()).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 400, "body {body}");
    }
    // The refusal names the problem, not just the status.
    let resp = launch("{\"harness\":\"kimi\",\"cwd\":\"/work/proj\"}".to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let problem: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(problem["detail"], "unknown harness", "{problem}");
    assert_eq!(problem["status"], 400, "{problem}");
    // All five harnesses are accepted.
    for h in ["claude", "codex", "opencode", "grok", "omp"] {
        let resp = launch(format!("{{\"harness\":\"{h}\",\"cwd\":\"/work/proj\"}}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 201, "harness {h}");
    }
}

/// POST `/api/terms` with the SPA's operator-intent headers.
async fn post_launch(
    c: &reqwest::Client,
    addr: std::net::SocketAddr,
    body: &str,
) -> reqwest::Response {
    c.post(format!("{}/api/terms", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn handoff_launch_forwards_handoff_from_to_the_runner() {
    let mut replies = std::collections::HashMap::new();
    replies.insert(
        "launch".to_string(),
        ok_reply("{\"id\":\"abcd1234\",\"harness\":\"claude\"}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = post_launch(
        &c,
        addr,
        "{\"harness\":\"claude\",\"cwd\":\"/work/proj\",\"handoff_from\":\"019e-abc_1.2:x\"}",
    )
    .await;
    assert_eq!(resp.status().as_u16(), 201);
    let reqs = fake.reqs.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert_eq!(
        reqs[0],
        serde_json::json!({
            "op": "launch", "harness": "claude", "argv": null, "resume_sid": null,
            "cwd": "/work/proj", "cols": null, "rows": null, "env": null,
            "exit_keys": null, "handoff_from": "019e-abc_1.2:x",
        })
    );
    // The omp target is accepted too.
    let resp = post_launch(
        &c,
        addr,
        "{\"harness\":\"omp\",\"cwd\":\"/work/proj\",\"handoff_from\":\"sid-1\"}",
    )
    .await;
    assert_eq!(resp.status().as_u16(), 201);
}

#[tokio::test]
async fn handoff_launch_without_handoff_from_forwards_no_key() {
    let mut replies = std::collections::HashMap::new();
    replies.insert(
        "launch".to_string(),
        ok_reply("{\"id\":\"abcd1234\",\"harness\":\"claude\"}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = post_launch(&c, addr, "{\"harness\":\"claude\",\"cwd\":\"/work/proj\"}").await;
    assert_eq!(resp.status().as_u16(), 201);
    let reqs = fake.reqs.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1, "{reqs:?}");
    assert!(
        reqs[0].get("handoff_from").is_none(),
        "no handoff_from key expected: {}",
        reqs[0]
    );
}

#[tokio::test]
async fn handoff_launch_validation_rejects_before_the_runner() {
    let mut replies = std::collections::HashMap::new();
    replies.insert(
        "launch".to_string(),
        ok_reply("{\"id\":\"abcd1234\",\"harness\":\"claude\"}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let too_long = format!(
        "{{\"harness\":\"claude\",\"cwd\":\"/work/proj\",\"handoff_from\":\"{}\"}}",
        "a".repeat(129)
    );
    for body in [
        // Only claude/omp are handoff targets.
        "{\"harness\":\"codex\",\"cwd\":\"/work/proj\",\"handoff_from\":\"sid-1\"}".to_string(),
        // Mutually exclusive with resume_sid.
        "{\"harness\":\"claude\",\"cwd\":\"/work/proj\",\"handoff_from\":\"sid-1\",\"resume_sid\":\"other\"}"
            .to_string(),
        // cwd is required ...
        "{\"harness\":\"claude\",\"handoff_from\":\"sid-1\"}".to_string(),
        // ... and absolute.
        "{\"harness\":\"claude\",\"cwd\":\"rel/path\",\"handoff_from\":\"sid-1\"}".to_string(),
        // Not a plain token.
        "{\"harness\":\"claude\",\"cwd\":\"/work/proj\",\"handoff_from\":\"a b\"}".to_string(),
        "{\"harness\":\"claude\",\"cwd\":\"/work/proj\",\"handoff_from\":\"a;rm -rf\"}"
            .to_string(),
        "{\"harness\":\"claude\",\"cwd\":\"/work/proj\",\"handoff_from\":\"-rf\"}".to_string(),
        "{\"harness\":\"claude\",\"cwd\":\"/work/proj\",\"handoff_from\":\"../x\"}".to_string(),
        "{\"harness\":\"claude\",\"cwd\":\"/work/proj\",\"handoff_from\":\"\"}".to_string(),
        too_long,
    ] {
        let resp = post_launch(&c, addr, &body).await;
        assert_eq!(resp.status().as_u16(), 400, "body {body}");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "bad-request");
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0, "0 runner connections");
}

#[tokio::test]
async fn paste_over_32kib_is_413_and_unknown_field_is_400() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    // > 32 KiB decoded → 413.
    let big = "a".repeat(33 * 1024);
    let body = format!("{{\"text\":\"{big}\"}}");
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/paste", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413);
    // Unknown field → 400.
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/paste", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{\"text\":\"hi\",\"unknown\":1}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "bad-request");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runner_errors_map_to_statuses() {
    async fn run(code: &str, want: u16) {
        let mut replies = std::collections::HashMap::new();
        replies.insert(
            "kill".to_string(),
            format!("{{\"status\":\"err\",\"code\":\"{code}\",\"message\":\"synthetic {code}\"}}"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let fake = spawn_fake(tmp.path(), replies);
        let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
        let c = client();
        let resp = c
            .post(format!("{}/api/terms/{TERM_ID}/kill", base_url(addr)))
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), want, "code {code}");
    }
    run("blocked", 409).await;
    run("multi-line-refused", 422).await;
    run("grid-owned", 409).await;
    run("paste-only", 409).await;
    run("not-found", 404).await;
    run("no-session", 404).await;
    run("no-viewer", 404).await;
    run("bad-request", 400).await;
    run("invalid-id", 400).await;
    run("too-large", 413).await;
    run("tap-timeout", 503).await;
    run("internal", 502).await;
    run("weird_code", 502).await;

    // Socket absent → 503, body does not contain the socket path.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("absent.sock");
    let fake = FakeRunner {
        conns: Arc::new(AtomicUsize::new(0)),
        reqs: Arc::new(Mutex::new(Vec::new())),
        socket: missing.clone(),
    };
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/kill", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    let (_s, text) = body_text(resp).await;
    assert!(!text.contains(&missing.to_string_lossy().into_owned()));
    assert_problem(&text, "terminal-runner-unavailable");

    // Malformed (unparseable) reply from a reachable runner → 502
    // `terminal-bad-reply`, spec §5 "anything else → 502" (only
    // unreachable/timeout is 503).
    let mut replies = std::collections::HashMap::new();
    replies.insert("kill".to_string(), "not json at all".to_string());
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/kill", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-bad-reply");
}

// ===== FXINTRUN integration fix-up: viewer lifecycle, composer guard,
// payload shapes, paste boundary, and the grid/viewer routes. =====

/// A stateful fake runner: `viewer_open`/`viewer_close` track
/// registered viewers; `paste`/`enter`/`keys` refuse with `no-viewer`
/// unless the viewer is registered; a paste starting with `/` or `@`
/// sets the session's `paste_only` flag until a keys/enter clears it;
/// `enter` with `from_composer: true` while `paste_only` is set
/// answers `paste-only`. This reproduces the runner's T9 behaviour
/// so the HTTP relay's lazy viewer open and composer flag are pinned
/// end-to-end.
fn spawn_fake_stateful(dir: &std::path::Path) -> FakeRunner {
    use std::collections::HashSet;
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let reqs: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let counter = conns.clone();
    let sink = reqs.clone();
    #[derive(Default)]
    struct State {
        viewers: HashSet<(String, String)>,
        paste_only: HashSet<String>,
    }
    let state = Arc::new(Mutex::new(State::default()));
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let sink = sink.clone();
            let state = state.clone();
            tokio::spawn(async move {
                let mut line = String::new();
                let mut reader = tokio::io::BufReader::new(stream);
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let parsed: serde_json::Value =
                    serde_json::from_str(line.trim()).unwrap_or_default();
                let op = parsed
                    .get("op")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                sink.lock().unwrap().push(parsed.clone());
                let reply = {
                    let mut st = state.lock().unwrap();
                    match op.as_str() {
                        "list" => list_reply(&[TERM_ID]),
                        "viewer_open" => {
                            let id = parsed["id"].as_str().unwrap_or_default().to_string();
                            let viewer = parsed["viewer"].as_str().unwrap_or_default().to_string();
                            st.viewers.insert((id, viewer.clone()));
                            format!("{{\"status\":\"ok\",\"data\":{{\"viewer\":\"{viewer}\"}}}}")
                        }
                        "viewer_close" => {
                            let id = parsed["id"].as_str().unwrap_or_default().to_string();
                            let viewer = parsed["viewer"].as_str().unwrap_or_default().to_string();
                            if st.viewers.remove(&(id.clone(), viewer.clone())) {
                                format!(
                                    "{{\"status\":\"ok\",\"data\":{{\"viewer\":\"{viewer}\"}}}}"
                                )
                            } else {
                                "{\"status\":\"err\",\"code\":\"no-viewer\",\"message\":\"synthetic no-viewer\"}".to_string()
                            }
                        }
                        "paste" => {
                            let id = parsed["id"].as_str().unwrap_or_default().to_string();
                            let viewer = parsed["viewer"].as_str().unwrap_or_default().to_string();
                            if !st.viewers.contains(&(id.clone(), viewer.clone())) {
                                "{\"status\":\"err\",\"code\":\"no-viewer\",\"message\":\"synthetic no-viewer\"}".to_string()
                            } else {
                                let text = parsed["text"].as_str().unwrap_or_default();
                                let paste_only = text.starts_with('/') || text.starts_with('@');
                                if paste_only {
                                    st.paste_only.insert(id);
                                }
                                format!(
                                    "{{\"status\":\"ok\",\"data\":{{\"paste_only\":{paste_only},\"bracketed\":true}}}}"
                                )
                            }
                        }
                        "keys" => {
                            let id = parsed["id"].as_str().unwrap_or_default().to_string();
                            let viewer = parsed["viewer"].as_str().unwrap_or_default().to_string();
                            if !st.viewers.contains(&(id.clone(), viewer.clone())) {
                                "{\"status\":\"err\",\"code\":\"no-viewer\",\"message\":\"synthetic no-viewer\"}".to_string()
                            } else {
                                st.paste_only.remove(&id);
                                "{\"status\":\"ok\",\"data\":{}}".to_string()
                            }
                        }
                        "enter" => {
                            let id = parsed["id"].as_str().unwrap_or_default().to_string();
                            let viewer = parsed["viewer"].as_str().unwrap_or_default().to_string();
                            if !st.viewers.contains(&(id.clone(), viewer.clone())) {
                                "{\"status\":\"err\",\"code\":\"no-viewer\",\"message\":\"synthetic no-viewer\"}".to_string()
                            } else if parsed["from_composer"].as_bool().unwrap_or(false)
                                && st.paste_only.contains(&id)
                            {
                                "{\"status\":\"err\",\"code\":\"paste-only\",\"message\":\"synthetic paste-only\"}".to_string()
                            } else {
                                st.paste_only.remove(&id);
                                "{\"status\":\"ok\",\"data\":{}}".to_string()
                            }
                        }
                        "kill" => {
                            "{\"status\":\"ok\",\"data\":{\"exit_code\":null,\"forced\":false}}"
                                .to_string()
                        }
                        _ => "{\"status\":\"ok\",\"data\":{}}".to_string(),
                    }
                };
                let mut stream = reader.into_inner();
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(b"\n").await;
            });
        }
    });
    FakeRunner {
        conns,
        reqs,
        socket: sock,
    }
}

async fn post_json(addr: std::net::SocketAddr, suffix: &str, body: &str) -> reqwest::Response {
    client()
        .post(format!("{}/api/terms{}", base_url(addr), suffix))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn input_opens_the_spa_viewer_lazily_and_composer_enter_is_refused_after_paste_only() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake_stateful(tmp.path());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    // No viewer was ever opened: the first input call must still work
    // because the relay opens the `spa` viewer idempotently first.
    let resp = post_json(addr, &format!("/{TERM_ID}/keys"), "{\"keys\":[\"C-c\"]}").await;
    assert_eq!(resp.status().as_u16(), 200);
    // A composer paste (text starting with `/`) sets paste_only; the
    // next composer Enter must be refused (409) with nothing written.
    let resp = post_json(
        addr,
        &format!("/{TERM_ID}/paste"),
        "{\"text\":\"/refresh\"}",
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let resp = post_json(addr, &format!("/{TERM_ID}/enter"), "{}").await;
    assert_eq!(resp.status().as_u16(), 409);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-conflict");
    // The enter carried `from_composer: true` and the `spa` viewer.
    let reqs = fake.reqs.lock().unwrap();
    let ops: Vec<&str> = reqs.iter().map(|r| r["op"].as_str().unwrap()).collect();
    assert!(
        ops.first() == Some(&"viewer_open"),
        "first op must be the lazy viewer_open, got {ops:?}"
    );
    let enter = reqs.iter().find(|r| r["op"] == "enter").unwrap();
    assert_eq!(enter["from_composer"], true);
    assert_eq!(enter["viewer"], "spa");
    assert_eq!(enter["id"], TERM_ID);
    let paste = reqs.iter().find(|r| r["op"] == "paste").unwrap();
    assert_eq!(paste["viewer"], "spa");
    assert_eq!(paste["text"], "/refresh");
}

#[tokio::test]
async fn paste_with_enter_is_one_request_that_pastes_then_enters() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake_stateful(tmp.path());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    // `enter: true`: ONE HTTP request, the runner sees paste then enter.
    let resp = post_json(
        addr,
        &format!("/{TERM_ID}/paste"),
        "{\"text\":\"hello\",\"enter\":true}",
    )
    .await;
    let (status, text) = body_text(resp).await;
    assert_eq!(status, 200, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["entered"], true, "{text}");
    assert_eq!(v["paste_only"], false);
    assert_eq!(v["bracketed"], true);
    assert!(v.get("enter_error").is_none(), "{text}");
    {
        let reqs = fake.reqs.lock().unwrap();
        let ops: Vec<&str> = reqs
            .iter()
            .map(|r| r["op"].as_str().unwrap())
            .filter(|o| *o == "paste" || *o == "enter")
            .collect();
        assert_eq!(ops, ["paste", "enter"]);
        let enter = reqs.iter().find(|r| r["op"] == "enter").unwrap();
        assert_eq!(enter["from_composer"], true);
        assert_eq!(enter["viewer"], "spa");
    }
    // Without the option the answer carries no `entered` and no enter is sent.
    let before = fake.reqs.lock().unwrap().len();
    let resp = post_json(addr, &format!("/{TERM_ID}/paste"), "{\"text\":\"plain\"}").await;
    let (status, text) = body_text(resp).await;
    assert_eq!(status, 200, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(v.get("entered").is_none(), "{text}");
    let reqs = fake.reqs.lock().unwrap();
    assert!(
        reqs[before..].iter().all(|r| r["op"] != "enter"),
        "a plain paste must not press Enter"
    );
}

#[tokio::test]
async fn paste_with_enter_keeps_pasted_not_sent_distinct_from_a_failed_paste() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake_stateful(tmp.path());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    // The paste lands (a leading `/` is paste_only), the Enter is refused:
    // 200 with `entered: false` and the refusal, never an error status.
    let resp = post_json(
        addr,
        &format!("/{TERM_ID}/paste"),
        "{\"text\":\"/refresh\",\"enter\":true}",
    )
    .await;
    let (status, text) = body_text(resp).await;
    assert_eq!(status, 200, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["paste_only"], true);
    assert_eq!(v["entered"], false, "{text}");
    assert_eq!(v["enter_error"]["status"], 409, "{text}");
    assert!(
        v["enter_error"]["type"]
            .as_str()
            .unwrap()
            .ends_with("terminal-conflict"),
        "{text}"
    );
    assert!(v["enter_error"]["detail"].as_str().is_some());
}

#[tokio::test]
async fn relay_payloads_carry_the_requested_fields() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("launch".to_string(), ok_reply("{\"id\":\"abcd1234\"}"));
    replies.insert(
        "take_size".to_string(),
        ok_reply("{\"cols\":80,\"rows\":24,\"owner\":\"kb-term\"}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    // Launch without cols/rows: the relay must send nulls so the
    // runner's own 120x40 default applies.
    let resp = post_json(addr, "", "{\"harness\":\"omp\",\"cwd\":\"/work/proj\"}").await;
    assert_eq!(resp.status().as_u16(), 201);
    // Launch with cols/rows passes them through.
    let resp = post_json(
        addr,
        "",
        "{\"harness\":\"omp\",\"cwd\":\"/work/proj\",\"cols\":100,\"rows\":30}",
    )
    .await;
    assert_eq!(resp.status().as_u16(), 201);
    // take-size must NOT force: a foreign grid owner must surface
    // `grid-owned` instead of being stomped.
    let resp = post_json(
        addr,
        &format!("/{TERM_ID}/take-size"),
        "{\"cols\":80,\"rows\":24}",
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
    let reqs = fake.reqs.lock().unwrap();
    let launches: Vec<&serde_json::Value> = reqs.iter().filter(|r| r["op"] == "launch").collect();
    assert_eq!(launches.len(), 2);
    assert!(
        launches[0]["cols"].is_null(),
        "omitted cols must be null: {}",
        launches[0]
    );
    assert!(
        launches[0]["rows"].is_null(),
        "omitted rows must be null: {}",
        launches[0]
    );
    assert_eq!(launches[1]["cols"], 100);
    assert_eq!(launches[1]["rows"], 30);
    let take = reqs.iter().find(|r| r["op"] == "take_size").unwrap();
    assert_eq!(take["force"], false, "take-size must send force:false");
    assert_eq!(take["owner"], "kb-term");
    assert_eq!(take["id"], TERM_ID);
    assert_eq!(take["cols"], 80);
    assert_eq!(take["rows"], 24);
}

#[tokio::test]
async fn every_relay_carries_the_exact_runner_request() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    replies.insert("launch".to_string(), ok_reply("{\"id\":\"abcd1234\"}"));
    replies.insert("viewer_open".to_string(), ok_reply("{\"viewer\":\"spa\"}"));
    replies.insert("viewer_close".to_string(), ok_reply("{\"viewer\":\"spa\"}"));
    replies.insert(
        "paste".to_string(),
        ok_reply("{\"paste_only\":false,\"bracketed\":true}"),
    );
    replies.insert("enter".to_string(), ok_reply("{}"));
    replies.insert("keys".to_string(), ok_reply("{}"));
    replies.insert(
        "kill".to_string(),
        ok_reply("{\"exit_code\":null,\"forced\":false}"),
    );
    replies.insert(
        "take_size".to_string(),
        ok_reply("{\"cols\":80,\"rows\":24,\"owner\":\"kb-term\"}"),
    );
    replies.insert("read".to_string(), ok_reply("{\"text\":\"\"}"));
    replies.insert("history".to_string(), ok_reply("{\"text\":\"\"}"));
    replies.insert(
        "grid".to_string(),
        ok_reply("{\"cols\":120,\"rows\":40,\"owner\":null}"),
    );
    replies.insert("release_size".to_string(), ok_reply("{}"));
    replies.insert("resize".to_string(), ok_reply("{}"));
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;

    // Drive one route, return exactly the runner requests it produced.
    async fn drive(
        fake: &FakeRunner,
        addr: std::net::SocketAddr,
        method: &str,
        suffix: &str,
        body: &str,
    ) -> Vec<serde_json::Value> {
        let before = fake.reqs.lock().unwrap().len();
        let url = format!("{}/api/terms{suffix}", base_url(addr));
        let req = if method == "GET" {
            client().get(url)
        } else {
            client()
                .post(url)
                .header("Content-Type", "application/json")
                .body(body.to_string())
        };
        let resp = req
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .send()
            .await
            .unwrap();
        let status = resp.status().as_u16();
        assert!(
            (200..300).contains(&status),
            "{method} {suffix} must succeed, got {status}"
        );
        fake.reqs.lock().unwrap()[before..].to_vec()
    }
    let id = TERM_ID;
    let open = serde_json::json!({"op": "viewer_open", "id": id, "viewer": "spa"});

    assert_eq!(
        drive(&fake, addr, "GET", "", "").await,
        vec![serde_json::json!({"op": "list"})]
    );
    assert_eq!(
        drive(
            &fake,
            addr,
            "POST",
            "",
            "{\"harness\":\"omp\",\"cwd\":\"/work/proj\"}"
        )
        .await,
        vec![serde_json::json!({
            "op": "launch", "harness": "omp", "argv": null, "resume_sid": null,
            "cwd": "/work/proj", "cols": null, "rows": null, "env": null,
            "exit_keys": null,
        })]
    );
    assert_eq!(
        drive(&fake, addr, "POST", &format!("/{id}/ticket"), "{}").await,
        vec![serde_json::json!({"op": "list"})]
    );
    assert_eq!(
        drive(
            &fake,
            addr,
            "POST",
            &format!("/{id}/paste"),
            "{\"text\":\"hello\"}"
        )
        .await,
        vec![
            open.clone(),
            serde_json::json!({"op": "paste", "id": id, "viewer": "spa", "text": "hello"}),
        ]
    );
    assert_eq!(
        drive(&fake, addr, "POST", &format!("/{id}/enter"), "{}").await,
        vec![
            open.clone(),
            serde_json::json!({
                "op": "enter", "id": id, "viewer": "spa",
                "blocked": false, "from_composer": true,
            }),
        ]
    );
    assert_eq!(
        drive(
            &fake,
            addr,
            "POST",
            &format!("/{id}/keys"),
            "{\"keys\":[\"C-c\",\"Enter\"]}"
        )
        .await,
        vec![
            open.clone(),
            serde_json::json!({
                "op": "keys", "id": id, "viewer": "spa",
                "input": {"kind": "named", "named": ["C-c", "Enter"]},
            }),
        ]
    );
    assert_eq!(
        drive(
            &fake,
            addr,
            "POST",
            &format!("/{id}/take-size"),
            "{\"cols\":80,\"rows\":24}"
        )
        .await,
        vec![serde_json::json!({
            "op": "take_size", "id": id, "cols": 80, "rows": 24,
            "owner": "kb-term", "force": false,
        })]
    );
    assert_eq!(
        drive(&fake, addr, "GET", &format!("/{id}/read"), "").await,
        vec![serde_json::json!({"op": "read", "id": id})]
    );
    assert_eq!(
        drive(&fake, addr, "GET", &format!("/{id}/history"), "").await,
        vec![serde_json::json!({"op": "history", "id": id})]
    );
    assert_eq!(
        drive(&fake, addr, "GET", &format!("/{id}/grid"), "").await,
        vec![serde_json::json!({"op": "grid", "id": id})]
    );
    assert_eq!(
        drive(&fake, addr, "POST", &format!("/{id}/release-size"), "{}").await,
        vec![serde_json::json!({"op": "release_size", "id": id, "owner": "kb-term"})]
    );
    assert_eq!(
        drive(
            &fake,
            addr,
            "POST",
            &format!("/{id}/resize"),
            "{\"cols\":100,\"rows\":30}"
        )
        .await,
        vec![serde_json::json!({"op": "resize", "id": id, "cols": 100, "rows": 30})]
    );
    assert_eq!(
        drive(&fake, addr, "POST", &format!("/{id}/viewer-close"), "{}").await,
        vec![serde_json::json!({"op": "viewer_close", "id": id, "viewer": "spa"})]
    );
    assert_eq!(
        drive(&fake, addr, "POST", &format!("/{id}/kill"), "{}").await,
        vec![
            serde_json::json!({"op": "kill", "id": id, "grace_secs": null}),
            serde_json::json!({"op": "viewer_close", "id": id, "viewer": "spa"}),
        ]
    );
}

#[tokio::test]
async fn paste_text_boundary_is_exactly_32kib() {
    let mut replies = std::collections::HashMap::new();
    replies.insert("viewer_open".to_string(), ok_reply("{\"viewer\":\"spa\"}"));
    replies.insert(
        "paste".to_string(),
        ok_reply("{\"paste_only\":false,\"bracketed\":true}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    async fn paste(addr: std::net::SocketAddr, text: String) -> u16 {
        let body = format!("{{\"text\":\"{text}\"}}");
        post_json(addr, &format!("/{TERM_ID}/paste"), &body)
            .await
            .status()
            .as_u16()
    }
    assert_eq!(
        paste(addr, "a".repeat(32 * 1024)).await,
        200,
        "exactly 32 KiB passes"
    );
    assert_eq!(
        paste(addr, "a".repeat(32 * 1024 + 1)).await,
        413,
        "32 KiB + 1 fails"
    );
    // `é` is 2 bytes: 16384 of them are exactly 32 KiB of bytes.
    assert_eq!(
        paste(addr, "é".repeat(16384)).await,
        200,
        "multibyte exact boundary"
    );
    assert_eq!(
        paste(addr, "é".repeat(16385)).await,
        413,
        "multibyte one over"
    );
}

#[tokio::test]
async fn grid_release_resize_and_viewer_close_relay_to_the_runner() {
    let mut replies = std::collections::HashMap::new();
    replies.insert(
        "grid".to_string(),
        ok_reply("{\"cols\":120,\"rows\":40,\"owner\":null}"),
    );
    replies.insert("release_size".to_string(), ok_reply("{}"));
    replies.insert("resize".to_string(), ok_reply("{}"));
    replies.insert("viewer_close".to_string(), ok_reply("{\"viewer\":\"spa\"}"));
    replies.insert("viewer_open".to_string(), ok_reply("{\"viewer\":\"spa\"}"));
    replies.insert(
        "kill".to_string(),
        ok_reply("{\"exit_code\":null,\"forced\":false}"),
    );
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    // GET /grid.
    let resp = client()
        .get(format!("{}/api/terms/{TERM_ID}/grid", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    // POST /release-size, /resize, /viewer-close.
    for (path, body) in [
        (format!("/{TERM_ID}/release-size"), "{}"),
        (format!("/{TERM_ID}/resize"), "{\"cols\":80,\"rows\":24}"),
        (format!("/{TERM_ID}/viewer-close"), "{}"),
    ] {
        let resp = post_json(addr, &path, body).await;
        assert_eq!(resp.status().as_u16(), 200, "path {path}");
    }
    // A successful kill also closes the `spa` viewer best-effort.
    let resp = post_json(addr, &format!("/{TERM_ID}/kill"), "{}").await;
    assert_eq!(resp.status().as_u16(), 200);
    let reqs = fake.reqs.lock().unwrap();
    let ops: Vec<&str> = reqs.iter().map(|r| r["op"].as_str().unwrap()).collect();
    for op in ["grid", "release_size", "resize", "viewer_close"] {
        assert!(ops.contains(&op), "op {op} must relay, got {ops:?}");
    }
    let release = reqs.iter().find(|r| r["op"] == "release_size").unwrap();
    assert_eq!(release["owner"], "kb-term");
    let resize = reqs.iter().find(|r| r["op"] == "resize").unwrap();
    assert_eq!(resize["cols"], 80);
    assert_eq!(resize["rows"], 24);
    let close = reqs.iter().find(|r| r["op"] == "viewer_close").unwrap();
    assert_eq!(close["viewer"], "spa");
    // kill closes the viewer after success.
    let kill_pos = ops.iter().position(|o| *o == "kill").unwrap();
    assert!(
        ops[kill_pos + 1..].contains(&"viewer_close"),
        "kill must close the spa viewer afterwards, got {ops:?}"
    );
}

/// R6: an id inside the kb-side grammar (`^[a-z0-9]{1,32}$`, the single
/// validator in `terminal/binding.rs`) that the runner rejects
/// (`invalid-id`) or does not know (`not-found`) is relayed as the mapped
/// 400/404, never a 502, and the gate does reach the runner for it (no
/// kb-side 8..=12 narrowing).
#[tokio::test]
async fn kb_grammar_ids_the_runner_rejects_or_does_not_know_map_to_4xx_never_502() {
    for id in ["a", "ab12", "a".repeat(32).as_str()] {
        for (code, want) in [("invalid-id", 400u16), ("not-found", 404)] {
            let mut replies = std::collections::HashMap::new();
            replies.insert(
                "kill".to_string(),
                format!(
                    "{{\"status\":\"err\",\"code\":\"{code}\",\"message\":\"synthetic {code}\"}}"
                ),
            );
            let tmp = tempfile::tempdir().unwrap();
            let fake = spawn_fake(tmp.path(), replies);
            let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
            let resp = client()
                .post(format!("{}/api/terms/{id}/kill", base_url(addr)))
                .header("Origin", origin_header(addr))
                .header("x-kb-intent", "terminal")
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), want, "id {id} code {code}");
            assert_eq!(
                fake.conns.load(Ordering::SeqCst),
                1,
                "id {id} must reach the runner, not be refused kb-side"
            );
        }
    }
}

/// `tracing-subscriber` writer that retains every line the server logs.
#[derive(Clone)]
struct CaptureWriter {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.buf.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn ticket_and_paste_text_never_reach_the_tracing_output() {
    let capture = CaptureWriter {
        buf: Arc::new(Mutex::new(Vec::new())),
    };
    tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .try_init()
        .expect("the capture subscriber must install in the test process");
    tracing::info!("capture-test-marker");

    let mut replies = std::collections::HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    replies.insert("viewer_open".to_string(), ok_reply("{\"viewer\":\"spa\"}"));
    replies.insert("paste".to_string(), ok_reply("{\"pasted\":true}"));
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    const SENTINEL: &str = "synthetic-paste-sentinel-9f2c";

    // Mint a ticket and take it back out of the response body (the only
    // place it is allowed to be).
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/ticket", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let (_s, mint_text) = body_text(resp).await;
    let ticket = serde_json::from_str::<serde_json::Value>(&mint_text).unwrap()["ticket"]
        .as_str()
        .unwrap()
        .to_string();

    // Paste uniquely identifiable text through the relay.
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/paste", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body(format!("{{\"text\":\"{}\"}}", SENTINEL))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);

    // Everything the server (and this test process) logged while
    // serving the requests above.
    let logged = String::from_utf8_lossy(&capture.buf.lock().unwrap()).into_owned();
    assert!(
        logged.contains("capture-test-marker"),
        "the capture must retain log lines; got: {logged}"
    );
    assert!(
        !logged.contains(&ticket),
        "the ticket must never reach the log stream"
    );
    assert!(
        !logged.contains(SENTINEL),
        "paste text must never reach the log stream"
    );
}

/// Every GET route the gate covers, media included (FUMINSERVER: the
/// media route was missing from the Rust table).
fn every_get_suffix() -> Vec<String> {
    let mut v: Vec<String> = ["", "/abcd1234/read", "/abcd1234/history", "/abcd1234/grid"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    v.push(format!("/media/{}", "a".repeat(64)));
    // FXLS: the working-area listing and download routes take the same
    // `gate_read` gate, so every refusal row below covers them and asserts
    // the runner is never dialled.
    v.push("/abcd1234/ls".to_string());
    v.push("/abcd1234/file".to_string());
    v.push("/abcd1234/ls?path=sub&root=up".to_string());
    v.push("/abcd1234/file?path=a.txt".to_string());
    v
}

/// A disabled tier answers a bodiless 404 on every GET (media included)
/// even when the request carries NO intent header and no Origin: the
/// enabled check runs before the intent check, so a disabled daemon
/// never answers 403 (which would name the feature).
#[tokio::test]
async fn disabled_tier_get_without_intent_is_a_bodiless_404_not_403() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(TerminalSection::default(), &fake, false).await;
    let c = client();
    for suffix in every_get_suffix() {
        for with_origin in [false, true] {
            let mut req = c.get(format!("{}/api/terms{suffix}", base_url(addr)));
            if with_origin {
                req = req.header("Origin", origin_header(addr));
            }
            let resp = req.send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 404, "{suffix} origin={with_origin}");
            assert!(resp.headers().get("content-type").is_none(), "{suffix}");
            let (_s, text) = body_text(resp).await;
            assert!(text.is_empty(), "{suffix} must be bodiless, got: {text}");
        }
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

/// The refusal rows the table was missing, over every GET route including
/// the media route: an own-Origin request that carries a cross-site
/// `Sec-Fetch-Site` is refused (the Origin alone must not admit it), and
/// the media route refuses a missing intent, a foreign Origin and a token
/// caller like the other GETs. No refused GET dials the runner.
#[tokio::test]
async fn get_refusals_over_every_route_including_media_and_own_origin_cross_site() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, true).await;
    let c = client();
    let own = origin_header(addr);
    for suffix in every_get_suffix() {
        let get = || c.get(format!("{}/api/terms{suffix}", base_url(addr)));
        for site in ["cross-site", "same-site"] {
            let resp = get()
                .header("x-kb-intent", "terminal")
                .header("Origin", own.clone())
                .header("sec-fetch-site", site)
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status().as_u16(), 403, "{suffix} own origin {site}");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-origin-refused");
        }
        let resp = get().header("Origin", own.clone()).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{suffix} no intent");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-intent-required");
        let resp = get()
            .header("x-kb-intent", "terminal")
            .header("Origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{suffix} foreign origin");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-origin-refused");
        let resp = get()
            .header("x-kb-intent", "terminal")
            .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{suffix} token caller");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-token-refused");
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

/// The Sec-Fetch-Site refusal names its own cause: same problem URN and
/// 403, but a detail distinct from the Origin refusal's, on every GET.
#[tokio::test]
async fn sec_fetch_site_refusal_has_its_own_detail() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let detail_of = |text: &str| -> String {
        serde_json::from_str::<serde_json::Value>(text)
            .unwrap()
            .get("detail")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string()
    };
    for suffix in every_get_suffix() {
        let url = format!("{}/api/terms{suffix}", base_url(addr));
        let resp = c
            .get(&url)
            .header("x-kb-intent", "terminal")
            .header("sec-fetch-site", "cross-site")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{suffix}");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-origin-refused");
        let site_detail = detail_of(&text);
        assert!(
            site_detail.contains("Sec-Fetch-Site"),
            "{suffix}: the detail must name the header, got: {site_detail}"
        );
        let resp = c
            .get(&url)
            .header("x-kb-intent", "terminal")
            .header("Origin", "https://evil.example")
            .send()
            .await
            .unwrap();
        let (_s, text) = body_text(resp).await;
        let origin_detail = detail_of(&text);
        assert!(!origin_detail.contains("Sec-Fetch-Site"), "{origin_detail}");
        assert_ne!(site_detail, origin_detail, "{suffix}");
    }
}

/// Restores `KB_DEV_ORIGIN_ANY` on drop, even when an assert panics.
struct DevOriginAny(Option<std::ffi::OsString>);

impl Drop for DevOriginAny {
    fn drop(&mut self) {
        match self.0.take() {
            Some(v) => std::env::set_var("KB_DEV_ORIGIN_ANY", v),
            None => std::env::remove_var("KB_DEV_ORIGIN_ANY"),
        }
    }
}

/// `KB_DEV_ORIGIN_ANY=1` widens the daemon's CORS-style `origin_allowed`
/// for any localhost origin; it must NOT widen the terminal GET gate: a
/// foreign-port localhost Origin is still refused on every GET route.
#[tokio::test]
async fn kb_dev_origin_any_does_not_open_a_terminal_get() {
    let _restore = DevOriginAny(std::env::var_os("KB_DEV_ORIGIN_ANY"));
    std::env::set_var("KB_DEV_ORIGIN_ANY", "1");
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for suffix in every_get_suffix() {
        let resp = c
            .get(format!("{}/api/terms{suffix}", base_url(addr)))
            .header("x-kb-intent", "terminal")
            .header("Origin", "http://localhost:9999")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{suffix}");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-origin-refused");
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------------------
// kbt2 PQSTORE — `/api/term-prompts/{project}[/{item}]`, the daemon-owned
// per-project prompt queue. Same gate as every terminal route; the fake
// runner is only here so `boot` has a socket, and no prompt route may ever
// dial it.
// ---------------------------------------------------------------------------

const PQ_SENTINEL: &str = "synthetic-queue-sentinel-5d1e";
const PQ_ITEM: &str = "q_0123456789ab";

fn pq_url(addr: std::net::SocketAddr, suffix: &str) -> String {
    format!("{}/api/term-prompts{suffix}", base_url(addr))
}

/// A browser-shaped mutation: own Origin + intent + JSON.
fn pq_req(
    c: &reqwest::Client,
    method: reqwest::Method,
    addr: std::net::SocketAddr,
    suffix: &str,
) -> reqwest::RequestBuilder {
    c.request(method, pq_url(addr, suffix))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
}

/// A browser-shaped same-origin GET: intent only, no Origin.
fn pq_get(
    c: &reqwest::Client,
    addr: std::net::SocketAddr,
    suffix: &str,
) -> reqwest::RequestBuilder {
    c.get(pq_url(addr, suffix))
        .header("x-kb-intent", "terminal")
}

async fn pq_add(
    c: &reqwest::Client,
    addr: std::net::SocketAddr,
    project: &str,
    text: &str,
) -> serde_json::Value {
    let resp = pq_req(c, reqwest::Method::POST, addr, &format!("/{project}"))
        .body(serde_json::json!({ "text": text }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    resp.json().await.unwrap()
}

fn find_file(root: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    for ent in std::fs::read_dir(root).ok()?.flatten() {
        let p = ent.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

fn pq_ledger(tmp: &tempfile::TempDir, project: &str) -> std::path::PathBuf {
    find_file(tmp.path(), &format!("{project}.json")).expect("ledger file")
}

/// The four routes as `(method, suffix, body)`; PATCH/DELETE carry a valid
/// `If-Match` in the gate tests so a refusal can only come from the gate.
fn pq_routes() -> Vec<(reqwest::Method, String, Option<&'static str>)> {
    vec![
        (reqwest::Method::GET, "/demo".to_string(), None),
        (
            reqwest::Method::POST,
            "/demo".to_string(),
            Some("{\"text\":\"hi\"}"),
        ),
        (
            reqwest::Method::PATCH,
            format!("/demo/{PQ_ITEM}"),
            Some("{\"done\":true}"),
        ),
        (reqwest::Method::DELETE, format!("/demo/{PQ_ITEM}"), None),
    ]
}

#[tokio::test]
async fn disabled_by_default_returns_bodiless_404_on_every_term_prompts_route() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(TerminalSection::default(), &fake, false).await;
    let c = client();
    for (m, suffix, body) in pq_routes() {
        let mut req = pq_req(&c, m.clone(), addr, &suffix).header("If-Match", "\"0\"");
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 404, "{m} {suffix}");
        assert!(resp.headers().get("content-type").is_none(), "{m} {suffix}");
        assert_eq!(
            resp.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let (_s, text) = body_text(resp).await;
        assert!(text.is_empty(), "{m} {suffix} must be bodiless, got {text}");
    }
    // A disabled tier answers 404 even with no intent and no Origin.
    let resp = c.get(pq_url(addr, "/demo")).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
    assert!(
        find_file(_tmp.path(), "demo.json").is_none(),
        "a disabled tier writes nothing"
    );
}

#[tokio::test]
async fn every_term_prompts_route_enforces_intent_and_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for (m, suffix, body) in pq_routes() {
        let build = |origin: Option<&str>, intent: Option<&str>| {
            let mut req = c
                .request(m.clone(), pq_url(addr, &suffix))
                .header("Content-Type", "application/json")
                .header("If-Match", "\"0\"");
            if let Some(o) = origin {
                req = req.header("Origin", o);
            }
            if let Some(i) = intent {
                req = req.header("x-kb-intent", i);
            }
            if let Some(b) = body {
                req = req.body(b.to_string());
            }
            req
        };
        let own = origin_header(addr);
        // No intent, and a wrong intent, with a good Origin.
        for intent in [None, Some("nope")] {
            let resp = build(Some(&own), intent).send().await.unwrap();
            assert_eq!(
                resp.status().as_u16(),
                403,
                "{m} {suffix} intent {intent:?}"
            );
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-intent-required");
        }
        // A present wrong Origin (an artifact subdomain, a foreign host).
        // On a mutation the daemon-wide Origin middleware may answer first
        // with its own `origin-refused`; either way it is a 403 that never
        // reaches the handler's store.
        for origin in ["http://x.artifacts.localhost", "https://evil.example"] {
            let resp = build(Some(origin), Some("terminal")).send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 403, "{m} {suffix} origin {origin}");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "origin-refused");
        }
        // No Origin: refused on every mutation; a same-origin GET is the
        // one shape that legitimately omits it.
        let resp = build(None, Some("terminal")).send().await.unwrap();
        if m == reqwest::Method::GET {
            assert_eq!(
                resp.status().as_u16(),
                200,
                "GET without Origin is admitted"
            );
        } else {
            assert_eq!(resp.status().as_u16(), 403, "{m} {suffix} without Origin");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-origin-refused");
        }
    }
    // A cross-site fetch is refused on the GET.
    let resp = pq_get(&c, addr, "/demo")
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
    assert!(
        find_file(tmp.path(), "demo.json").is_none(),
        "a refused request must write nothing"
    );
}

#[tokio::test]
async fn term_prompts_refuse_token_callers_and_loopback_without_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, true).await;
    let c = client();
    for (m, suffix, body) in pq_routes() {
        // The shared (legacy) bearer token (a registry token takes the same
        // `Token` class in the gate's caller-class step):
        // the SPA-shaped request is otherwise perfect.
        let mut req = pq_req(&c, m.clone(), addr, &suffix)
            .header("If-Match", "\"0\"")
            .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"));
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{m} {suffix} via bearer");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-token-refused");
        // A loopback caller (this process) with neither Origin nor
        // intent: the auth bypass admits it, the gate must not.
        let mut req = c.request(m.clone(), pq_url(addr, &suffix));
        if let Some(b) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{m} {suffix} bare loopback");
    }
    assert!(find_file(tmp.path(), "demo.json").is_none());
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn term_prompts_if_match_flow_bounds_and_privacy_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();

    // A project with no file: rev 0, no items, an ETag, and nothing on disk.
    let resp = pq_get(&c, addr, "/demo").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"0\"")
    );
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["rev"], 0);
    assert_eq!(v["items"].as_array().unwrap().len(), 0);
    assert!(
        find_file(tmp.path(), "demo.json").is_none(),
        "a GET must not create the ledger"
    );

    // Two adds: 201, order 0 and 1, ETag follows the rev, no-store.
    let resp = pq_req(&c, reqwest::Method::POST, addr, "/demo")
        .body(serde_json::json!({ "text": PQ_SENTINEL }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    assert_eq!(
        resp.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"1\"")
    );
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["items"][0]["text"], PQ_SENTINEL);
    let second = pq_add(&c, addr, "demo", "second").await;
    assert_eq!(second["rev"], 2);
    assert_eq!(second["items"][1]["order"], 1);
    let first_id = second["items"][0]["id"].as_str().unwrap().to_string();
    let item_path = format!("/demo/{first_id}");
    let ledger = pq_ledger(&tmp, "demo");

    // PATCH without If-Match: 428, nothing written.
    let before = std::fs::read(&ledger).unwrap();
    let resp = pq_req(&c, reqwest::Method::PATCH, addr, &item_path)
        .body("{\"done\":true}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 428);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "precondition-required");
    assert_eq!(std::fs::read(&ledger).unwrap(), before);

    // A stale rev: 412 naming the current rev, file byte-identical.
    let resp = pq_req(&c, reqwest::Method::PATCH, addr, &item_path)
        .header("If-Match", "\"1\"")
        .body("{\"done\":true}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 412);
    assert_eq!(
        resp.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"2\"")
    );
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "prompts-stale");
    assert!(
        !text.contains(PQ_SENTINEL),
        "a problem body must not echo prompt text"
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), before);

    // The current rev: 200 and rev + 1.
    let resp = pq_req(&c, reqwest::Method::PATCH, addr, &item_path)
        .header("If-Match", "\"2\"")
        .body("{\"done\":true,\"position\":9999}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"3\"")
    );
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["rev"], 3);
    assert_eq!(
        v["items"][1]["id"], first_id,
        "position clamps to the last slot"
    );
    assert!(v["items"][1]["done_at"].is_i64());
    let after_patch = std::fs::read(&ledger).unwrap();

    // Replaying the same rev: 412.
    let resp = pq_req(&c, reqwest::Method::PATCH, addr, &item_path)
        .header("If-Match", "\"2\"")
        .body("{\"done\":false}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 412);
    // DELETE with a stale rev: 412, nothing removed.
    let resp = pq_req(&c, reqwest::Method::DELETE, addr, &item_path)
        .header("If-Match", "\"2\"")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 412);
    assert_eq!(std::fs::read(&ledger).unwrap(), after_patch);

    // A no-op PATCH (already done) still needs the rev, bumps nothing.
    let resp = pq_req(&c, reqwest::Method::PATCH, addr, &item_path)
        .header("If-Match", "\"3\"")
        .body("{\"done\":true}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers().get("etag").and_then(|v| v.to_str().ok()),
        Some("\"3\"")
    );
    assert_eq!(std::fs::read(&ledger).unwrap(), after_patch);

    // Validation: empty PATCH, unknown field, bad text, bad ids, bad slug,
    // oversized body, malformed If-Match.
    let patch = |body: &'static str, id: &str| {
        pq_req(&c, reqwest::Method::PATCH, addr, &format!("/demo/{id}"))
            .header("If-Match", "\"3\"")
            .body(body)
            .send()
    };
    for (body, want, urn) in [
        ("{}", 400, "bad-request"),
        ("{\"nope\":1}", 400, "bad-request"),
        ("{\"text\":\"  \"}", 400, "bad-text"),
        ("{\"position\":-1}", 400, "bad-request"),
    ] {
        let resp = patch(body, &first_id).await.unwrap();
        assert_eq!(resp.status().as_u16(), want, "{body}");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, urn);
    }
    assert_eq!(
        patch("{\"done\":false}", "q_000000000000")
            .await
            .unwrap()
            .status()
            .as_u16(),
        404
    );
    assert_eq!(
        patch("{\"done\":false}", "not-an-id")
            .await
            .unwrap()
            .status()
            .as_u16(),
        400
    );
    let resp = pq_req(&c, reqwest::Method::PATCH, addr, &item_path)
        .header("If-Match", "*")
        .body("{\"done\":false}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let too_long = "a".repeat(65);
    for bad in ["A", "..%2fx", too_long.as_str()] {
        let resp = pq_req(&c, reqwest::Method::POST, addr, &format!("/{bad}"))
            .body("{\"text\":\"x\"}")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 400, "slug {bad}");
    }
    let resp = pq_req(&c, reqwest::Method::POST, addr, "/demo")
        .body(format!("{{\"text\":\"{}\"}}", "a".repeat(16 * 1024)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413);
    // Text bounds over HTTP: 8192 passes (rev 4), 8193 is bad-text.
    let resp = pq_req(&c, reqwest::Method::POST, addr, "/demo")
        .body(format!("{{\"text\":\"{}\"}}", "a".repeat(8192)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let resp = pq_req(&c, reqwest::Method::POST, addr, "/demo")
        .body(format!("{{\"text\":\"{}\"}}", "a".repeat(8193)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "bad-text");

    // DELETE: unknown item 404; without If-Match is allowed; the answer is
    // the queue.
    let resp = pq_req(&c, reqwest::Method::DELETE, addr, "/demo/q_000000000000")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let resp = pq_req(&c, reqwest::Method::DELETE, addr, &item_path)
        .header("If-Match", "\"4\"")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["rev"], 5);
    assert!(v["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["id"] != first_id));

    // 100 items per project, done included: fill to the cap, then 409.
    for i in 0..(100 - 2) {
        let _ = pq_add(&c, addr, "full", &format!("p{i}")).await;
    }
    let _ = pq_add(&c, addr, "full", "p98").await;
    let _ = pq_add(&c, addr, "full", "p99").await;
    let resp = pq_req(&c, reqwest::Method::POST, addr, "/full")
        .body("{\"text\":\"over\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 409);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "queue-full");
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "the queue never dials the runner"
    );
}

#[tokio::test]
async fn twenty_concurrent_adds_lose_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let mut tasks = Vec::new();
    for i in 0..20 {
        let c = c.clone();
        tasks.push(tokio::spawn(async move {
            pq_add(&c, addr, "race", &format!("item {i}")).await
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let v: serde_json::Value = pq_get(&c, addr, "/race")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["rev"], 20, "every add bumped the revision exactly once");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 20);
    let mut ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 20, "20 distinct ids");
    let mut texts: Vec<&str> = items.iter().map(|i| i["text"].as_str().unwrap()).collect();
    texts.sort();
    texts.dedup();
    assert_eq!(texts.len(), 20, "no lost write");
}

/// One SSE frame per durable mutation, `{project, rev}` only, no text; a
/// 412 and a no-op PATCH emit nothing.
#[tokio::test]
async fn term_prompts_emit_exactly_one_text_free_event_per_durable_mutation() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let mut stream = c
        .get(format!("{}/api/events?types=term.prompts", base_url(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status().as_u16(), 200);
    let collector = tokio::spawn(async move {
        let mut raw = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(3000);
        loop {
            let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                break;
            }
            match tokio::time::timeout(remain, stream.chunk()).await {
                Ok(Ok(Some(b))) => raw.push_str(&String::from_utf8_lossy(&b)),
                _ => break,
            }
        }
        raw
    });

    let v = pq_add(&c, addr, "demo", PQ_SENTINEL).await; // rev 1
    let id = v["items"][0]["id"].as_str().unwrap().to_string();
    let path = format!("/demo/{id}");
    let ok = pq_req(&c, reqwest::Method::PATCH, addr, &path)
        .header("If-Match", "\"1\"")
        .body("{\"done\":true}") // rev 2
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    let stale = pq_req(&c, reqwest::Method::PATCH, addr, &path)
        .header("If-Match", "\"1\"")
        .body("{\"done\":false}") // 412: no event
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status().as_u16(), 412);
    let noop = pq_req(&c, reqwest::Method::PATCH, addr, &path)
        .header("If-Match", "\"2\"")
        .body("{\"done\":true}") // no-op: no event
        .send()
        .await
        .unwrap();
    assert_eq!(noop.status().as_u16(), 200);
    let del = pq_req(&c, reqwest::Method::DELETE, addr, &path)
        .send() // rev 3
        .await
        .unwrap();
    assert_eq!(del.status().as_u16(), 200);
    let refused = pq_req(&c, reqwest::Method::POST, addr, "/demo")
        .body("{\"text\":\"\"}") // 400: no event
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status().as_u16(), 400);

    let raw = collector.await.unwrap();
    assert!(
        !raw.contains(PQ_SENTINEL),
        "prompt text must never ride an event: {raw}"
    );
    let mut seen = Vec::new();
    for frame in raw.split("\n\n") {
        let mut event = "";
        let mut data = "";
        for line in frame.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event = v.trim();
            } else if let Some(v) = line.strip_prefix("data:") {
                data = v.trim();
            }
        }
        if event != "term.prompts" {
            continue;
        }
        let body: serde_json::Value = serde_json::from_str(data).unwrap();
        let payload = body["payload"].as_object().expect("payload object");
        let mut keys: Vec<&str> = payload.keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(keys, ["project", "rev"]);
        assert_eq!(payload["project"], "demo");
        seen.push(payload["rev"].as_u64().unwrap());
    }
    assert_eq!(
        seen,
        vec![1, 2, 3],
        "exactly one event per durable mutation: {raw}"
    );
}

// ---------------------------------------------------------------------------
// kbt2 CMDSTORE — `/api/terms/commands`, the daemon-owned command-catalogue
// overlay. Same gate as every terminal route; the fake runner is only here so
// `boot` has a socket, and no catalogue route may ever dial it.
// ---------------------------------------------------------------------------

const CM_DEFAULTS: &str = include_str!("../../../web/src/terminal/commands/defaults.json");
const CM_SENTINEL: &str = "synthetic-catalogue-sentinel-9c2f";

fn cm_url(addr: std::net::SocketAddr) -> String {
    format!("{}/api/terms/commands", base_url(addr))
}

/// A browser-shaped mutation: own Origin + intent + JSON.
fn cm_req(
    c: &reqwest::Client,
    method: reqwest::Method,
    addr: std::net::SocketAddr,
) -> reqwest::RequestBuilder {
    c.request(method, cm_url(addr))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
}

/// A browser-shaped same-origin GET: intent only, no Origin.
fn cm_get(c: &reqwest::Client, addr: std::net::SocketAddr) -> reqwest::RequestBuilder {
    c.get(cm_url(addr)).header("x-kb-intent", "terminal")
}

fn cm_put(
    c: &reqwest::Client,
    addr: std::net::SocketAddr,
    if_match: &str,
    body: &str,
) -> reqwest::RequestBuilder {
    cm_req(c, reqwest::Method::PUT, addr)
        .header("If-Match", if_match)
        .body(body.to_string())
}

fn cm_file(tmp: &tempfile::TempDir) -> Option<std::path::PathBuf> {
    find_file(tmp.path(), "term-commands.json")
}

fn cm_routes() -> Vec<(reqwest::Method, Option<&'static str>)> {
    vec![
        (reqwest::Method::GET, None),
        (
            reqwest::Method::PUT,
            Some("{\"hidden\":{\"claude\":[\"help\"]}}"),
        ),
        (reqwest::Method::DELETE, None),
    ]
}

fn cm_etag(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

fn cm_no_store(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        == Some("no-store")
}

fn cm_item_ids(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    for (h, hv) in v["harnesses"].as_object().unwrap() {
        for g in hv["groups"].as_array().unwrap() {
            for it in g["items"].as_array().unwrap() {
                out.push(format!("{h}/{}", it["id"].as_str().unwrap()));
            }
        }
    }
    out
}

#[tokio::test]
async fn disabled_by_default_returns_bodiless_404_on_every_term_commands_route() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(TerminalSection::default(), &fake, false).await;
    let c = client();
    for (m, body) in cm_routes() {
        let mut req = cm_req(&c, m.clone(), addr).header("If-Match", "\"0\"");
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 404, "{m}");
        assert!(resp.headers().get("content-type").is_none(), "{m}");
        assert!(cm_no_store(&resp), "{m}");
        let (_s, text) = body_text(resp).await;
        assert!(text.is_empty(), "{m} must be bodiless, got {text}");
    }
    // A disabled tier answers 404 even with no intent and no Origin, and
    // even for an oversize body.
    let resp = c.get(cm_url(addr)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    let resp = cm_put(&c, addr, "\"0\"", &" ".repeat(70_000))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
    assert!(cm_file(&tmp).is_none(), "a disabled tier writes nothing");
}

#[tokio::test]
async fn every_term_commands_route_enforces_intent_and_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for (m, body) in cm_routes() {
        let build = |origin: Option<&str>, intent: Option<&str>| {
            let mut req = c
                .request(m.clone(), cm_url(addr))
                .header("Content-Type", "application/json")
                .header("If-Match", "\"0\"");
            if let Some(o) = origin {
                req = req.header("Origin", o);
            }
            if let Some(i) = intent {
                req = req.header("x-kb-intent", i);
            }
            if let Some(b) = body {
                req = req.body(b.to_string());
            }
            req
        };
        let own = origin_header(addr);
        for intent in [None, Some("nope")] {
            let resp = build(Some(&own), intent).send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 403, "{m} intent {intent:?}");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-intent-required");
        }
        for origin in ["http://x.artifacts.localhost", "https://evil.example"] {
            let resp = build(Some(origin), Some("terminal")).send().await.unwrap();
            assert_eq!(resp.status().as_u16(), 403, "{m} origin {origin}");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "origin-refused");
        }
        let resp = build(None, Some("terminal")).send().await.unwrap();
        if m == reqwest::Method::GET {
            assert_eq!(
                resp.status().as_u16(),
                200,
                "GET without Origin is admitted"
            );
        } else {
            assert_eq!(resp.status().as_u16(), 403, "{m} without Origin");
            let (_s, text) = body_text(resp).await;
            assert_problem(&text, "terminal-origin-refused");
        }
    }
    let resp = cm_get(&c, addr)
        .header("sec-fetch-site", "cross-site")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
    assert!(
        cm_file(&tmp).is_none(),
        "a refused request must write nothing"
    );
}

#[tokio::test]
async fn term_commands_refuse_token_callers_and_loopback_without_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, true).await;
    let c = client();
    for (m, body) in cm_routes() {
        let mut req = cm_req(&c, m.clone(), addr)
            .header("If-Match", "\"0\"")
            .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"));
        if let Some(b) = body {
            req = req.body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{m} via bearer");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-token-refused");
        let mut req = c.request(m.clone(), cm_url(addr));
        if let Some(b) = body {
            req = req
                .header("Content-Type", "application/json")
                .body(b.to_string());
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{m} bare loopback");
    }
    assert!(cm_file(&tmp).is_none());
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn term_commands_fresh_get_is_the_defaults_at_rev_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = cm_get(&c, addr).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(cm_etag(&resp).as_deref(), Some("\"0\""));
    assert!(cm_no_store(&resp));
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["rev"], 0);
    let defaults: serde_json::Value = serde_json::from_str(CM_DEFAULTS).unwrap();
    let mut want = Vec::new();
    for (h, hv) in defaults["harnesses"].as_object().unwrap() {
        for g in hv["groups"].as_array().unwrap() {
            for it in g["items"].as_array().unwrap() {
                want.push(format!("{h}/{}", it["id"].as_str().unwrap()));
            }
        }
    }
    assert_eq!(cm_item_ids(&v), want, "the shipped item ids, in order");
    for (_, hv) in v["harnesses"].as_object().unwrap() {
        for g in hv["groups"].as_array().unwrap() {
            assert_eq!(g["origin"], "default");
            for it in g["items"].as_array().unwrap() {
                assert_eq!(it["origin"], "default");
            }
        }
    }
    assert!(cm_file(&tmp).is_none(), "a GET must not create the file");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn term_commands_if_match_flow_validation_and_reset_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let edit = serde_json::json!({
        "harnesses": {"claude": {"groups": [{"id": "slash", "label": "Slash commands", "items": [
            {"id": "help", "kind": "command", "label": CM_SENTINEL, "text": "/help", "mode": "paste"},
            {"id": "reply-new", "kind": "reply", "label": "Mine", "text": "go on"}
        ]}]}}
    })
    .to_string();

    // PUT without If-Match: 428, nothing written.
    let resp = cm_req(&c, reqwest::Method::PUT, addr)
        .body(edit.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 428);
    assert!(cm_no_store(&resp));
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "precondition-required");
    assert!(cm_file(&tmp).is_none());
    // A malformed If-Match: 400.
    let resp = cm_put(&c, addr, "banana", &edit).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 400);

    // The current rev: 200, rev 1, `help` is now an overlay entry.
    let resp = cm_put(&c, addr, "\"0\"", &edit).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(cm_etag(&resp).as_deref(), Some("\"1\""));
    assert!(cm_no_store(&resp));
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["rev"], 1);
    let slash = v["harnesses"]["claude"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "slash")
        .unwrap();
    let help = slash["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "help")
        .unwrap();
    assert_eq!(help["origin"], "overlay");
    assert_eq!(help["label"], CM_SENTINEL);
    let compact = slash["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "compact")
        .unwrap();
    assert_eq!(compact["origin"], "default");
    let file = cm_file(&tmp).expect("overlay file");
    let after_put = std::fs::read(&file).unwrap();
    assert!(!file.with_extension("json.tmp").exists());

    // The same rev again: 412 naming the current rev, file byte-identical.
    let resp = cm_put(&c, addr, "\"0\"", &edit).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 412);
    assert_eq!(cm_etag(&resp).as_deref(), Some("\"1\""));
    let text = resp.text().await.unwrap();
    assert_problem(&text, "commands-stale");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["rev"], 1, "a typed rev in the 412 body");
    assert!(
        !text.contains(CM_SENTINEL),
        "a problem must not echo content"
    );
    assert_eq!(std::fs::read(&file).unwrap(), after_put);
    // A stale DELETE: 412, nothing removed.
    let resp = cm_req(&c, reqwest::Method::DELETE, addr)
        .header("If-Match", "\"0\"")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 412);
    assert_eq!(std::fs::read(&file).unwrap(), after_put);

    // An identical overlay with the current rev is a no-op: rev stays 1.
    let resp = cm_put(&c, addr, "\"1\"", &edit).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(cm_etag(&resp).as_deref(), Some("\"1\""));
    assert_eq!(std::fs::read(&file).unwrap(), after_put);

    // An invalid item: 400 `bad-catalogue` with the ordered defect list.
    let bad = serde_json::json!({"harnesses": {"claude": {"groups": [
        {"id": "g", "label": "G", "items": [
            {"id": "x", "kind": "command", "label": "X", "text": "a\nb", "mode": "run"}]}]}}})
    .to_string();
    let resp = cm_put(&c, addr, "\"1\"", &bad).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "bad-catalogue");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["errors"][0]["code"], "run-multiline");
    assert_eq!(
        body["errors"][0]["path"],
        "/harnesses/claude/groups/0/items/0/text"
    );
    // Not JSON, an unknown top-level key, a bad hidden id.
    let resp = cm_put(&c, addr, "\"1\"", "not json").send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let resp = cm_put(&c, addr, "\"1\"", "{\"rev\":9}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    let resp = cm_put(&c, addr, "\"1\"", "{\"hidden\":{\"claude\":[\"BAD ID\"]}}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 400);
    // A merge that breaks the item cap: 409, not a stored overlay.
    let items: Vec<serde_json::Value> = (0..200)
        .map(|i| serde_json::json!({"id": format!("n{i}"), "kind": "reply", "label": "n", "text": "n"}))
        .collect();
    let big = serde_json::json!({"harnesses": {"claude": {"groups": [
        {"id": "big", "label": "Big", "items": items}]}}})
    .to_string();
    let resp = cm_put(&c, addr, "\"1\"", &big).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 409);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "catalogue-too-large");
    assert_eq!(
        std::fs::read(&file).unwrap(),
        after_put,
        "every refusal leaves the file"
    );

    // DELETE: defaults only, and the rev keeps climbing (never back to 0).
    let resp = cm_req(&c, reqwest::Method::DELETE, addr)
        .header("If-Match", "\"1\"")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(cm_etag(&resp).as_deref(), Some("\"2\""));
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["rev"], 2);
    for (_, hv) in v["harnesses"].as_object().unwrap() {
        for g in hv["groups"].as_array().unwrap() {
            assert_eq!(g["origin"], "default");
            for it in g["items"].as_array().unwrap() {
                assert_eq!(it["origin"], "default");
            }
        }
    }
    let after_reset = std::fs::read(&file).unwrap();
    // A second DELETE is a no-op: rev stays 2, bytes unchanged.
    let resp = cm_req(&c, reqwest::Method::DELETE, addr)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(cm_etag(&resp).as_deref(), Some("\"2\""));
    assert_eq!(std::fs::read(&file).unwrap(), after_reset);
    // A GET agrees.
    let resp = cm_get(&c, addr).send().await.unwrap();
    assert_eq!(cm_etag(&resp).as_deref(), Some("\"2\""));
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "never dials the runner"
    );
}

#[tokio::test]
async fn term_commands_body_cap_is_exactly_64_kib() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let pad = |n: usize| format!("{{}}{}", " ".repeat(n - 2));
    let resp = cm_put(&c, addr, "\"0\"", &pad(65_536))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "exactly the cap is accepted");
    let resp = cm_put(&c, addr, "\"0\"", &pad(65_537))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413);
    assert!(cm_no_store(&resp));
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "payload-too-large");
    assert!(
        cm_file(&tmp).is_none(),
        "an empty overlay and a 413 write nothing"
    );
}

#[tokio::test]
async fn term_commands_corrupt_file_is_a_typed_500_and_left_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    // Land a real file first, then corrupt it in place.
    let resp = cm_put(&c, addr, "\"0\"", "{\"hidden\":{\"claude\":[\"help\"]}}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let file = cm_file(&tmp).unwrap();
    std::fs::write(&file, b"{\"schema\":\"kb-term-commands/9\",\"rev\":3}").unwrap();
    let before = std::fs::read(&file).unwrap();
    let resp = cm_get(&c, addr).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 500);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "commands-corrupt");
    let resp = cm_put(&c, addr, "\"3\"", "{\"hidden\":{\"claude\":[\"compact\"]}}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 500);
    let resp = cm_req(&c, reqwest::Method::DELETE, addr)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 500);
    assert_eq!(std::fs::read(&file).unwrap(), before, "never reset");
}

/// 20 writers, each carrying the rev it read: exactly one wins a round, the
/// other 19 get 412, the final rev counts the winners and the file is valid.
#[tokio::test]
async fn twenty_concurrent_term_commands_puts_exactly_one_wins_per_round() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let mut wins = 0u64;
    for round in 0..3u64 {
        let rev = cm_get(&c, addr)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()["rev"]
            .as_u64()
            .unwrap();
        assert_eq!(rev, wins);
        let mut tasks = Vec::new();
        for i in 0..20 {
            let c = c.clone();
            tasks.push(tokio::spawn(async move {
                let body = serde_json::json!({"hidden": {"claude": [format!("r{round}w{i}")]}})
                    .to_string();
                cm_put(&c, addr, &format!("\"{rev}\""), &body)
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16()
            }));
        }
        let mut ok = 0;
        let mut stale = 0;
        for t in tasks {
            match t.await.unwrap() {
                200 => ok += 1,
                412 => stale += 1,
                other => panic!("unexpected status {other}"),
            }
        }
        assert_eq!((ok, stale), (1, 19), "round {round}");
        wins += 1;
    }
    let v: serde_json::Value = cm_get(&c, addr).send().await.unwrap().json().await.unwrap();
    assert_eq!(v["rev"], wins);
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(cm_file(&tmp).unwrap()).unwrap()).unwrap();
    assert_eq!(doc["schema"], "kb-term-commands/1");
    assert_eq!(doc["rev"], wins);
}

/// One SSE frame per durable change, `{rev}` only and no catalogue text; a
/// 412, a 400, a no-op PUT and a no-op DELETE emit nothing.
#[tokio::test]
async fn term_commands_emit_exactly_one_content_free_event_per_durable_change() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let mut stream = c
        .get(format!("{}/api/events?types=term.commands", base_url(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status().as_u16(), 200);
    let collector = tokio::spawn(async move {
        let mut raw = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(3000);
        loop {
            let remain = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remain.is_zero() {
                break;
            }
            match tokio::time::timeout(remain, stream.chunk()).await {
                Ok(Ok(Some(b))) => raw.push_str(&String::from_utf8_lossy(&b)),
                _ => break,
            }
        }
        raw
    });

    let edit = serde_json::json!({"harnesses": {"omp": {"groups": [
        {"id": "mine", "label": CM_SENTINEL, "items": [
            {"id": "m1", "kind": "reply", "label": CM_SENTINEL, "text": CM_SENTINEL}]}]}}})
    .to_string();
    let ok = cm_put(&c, addr, "\"0\"", &edit).send().await.unwrap(); // rev 1
    assert_eq!(ok.status().as_u16(), 200);
    let stale = cm_put(&c, addr, "\"0\"", &edit).send().await.unwrap(); // 412
    assert_eq!(stale.status().as_u16(), 412);
    let noop = cm_put(&c, addr, "\"1\"", &edit).send().await.unwrap(); // no-op
    assert_eq!(noop.status().as_u16(), 200);
    let bad = cm_put(&c, addr, "\"1\"", "{\"rev\":1}")
        .send()
        .await
        .unwrap(); // 400
    assert_eq!(bad.status().as_u16(), 400);
    let del = cm_req(&c, reqwest::Method::DELETE, addr)
        .send()
        .await
        .unwrap(); // rev 2
    assert_eq!(del.status().as_u16(), 200);
    let noop_del = cm_req(&c, reqwest::Method::DELETE, addr)
        .send()
        .await
        .unwrap();
    assert_eq!(noop_del.status().as_u16(), 200);

    let raw = collector.await.unwrap();
    assert!(
        !raw.contains(CM_SENTINEL),
        "catalogue text must never ride an event: {raw}"
    );
    let mut seen = Vec::new();
    for frame in raw.split("\n\n") {
        let mut event = "";
        let mut data = "";
        for line in frame.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                event = v.trim();
            } else if let Some(v) = line.strip_prefix("data:") {
                data = v.trim();
            }
        }
        if event != "term.commands" {
            continue;
        }
        let body: serde_json::Value = serde_json::from_str(data).unwrap();
        let payload = body["payload"].as_object().expect("payload object");
        let keys: Vec<&str> = payload.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, ["rev"]);
        seen.push(payload["rev"].as_u64().unwrap());
    }
    assert_eq!(
        seen,
        vec![1, 2],
        "exactly one event per durable change: {raw}"
    );
}

// ---------------------------------------------------------------- FXUP
// `POST /api/terms/{id}/files` runs the same handler gate as every other
// terminal POST, before any id, query, size or length handling.

fn files_url(addr: std::net::SocketAddr, id: &str) -> String {
    format!("{}/api/terms/{id}/files?name=a.pdf", base_url(addr))
}

#[tokio::test]
async fn files_route_disabled_is_a_bodiless_404_even_for_an_oversize_length() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(TerminalSection::default(), &fake, false).await;
    for len in [64usize, 33 * 1024 * 1024] {
        let resp = client()
            .post(files_url(addr, TERM_ID))
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .header("Content-Length", len.to_string())
            .body(vec![0u8; 64])
            .send()
            .await;
        // An oversize declared length with a short body may be cut by the
        // client; only the answered case carries assertions.
        if let Ok(resp) = resp {
            assert_eq!(resp.status().as_u16(), 404, "len {len}");
            assert!(resp.headers().get("content-type").is_none());
            let (_s, text) = body_text(resp).await;
            assert!(text.is_empty(), "bodiless, got: {text}");
        }
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn files_route_gate_matrix_refuses_before_the_runner_is_dialled() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, true).await;
    let c = client();
    let url = files_url(addr, TERM_ID);

    // No Origin (loopback does not bypass), with and without intent.
    let resp = c
        .post(&url)
        .header("x-kb-intent", "terminal")
        .body(vec![1u8; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-origin-refused");

    // A wrong Origin: refused (daemon allowlist or terminal gate, both end
    // in `origin-refused`).
    let resp = c
        .post(&url)
        .header("Origin", "https://evil.example")
        .header("x-kb-intent", "terminal")
        .body(vec![1u8; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "origin-refused");

    // Missing and wrong intent.
    for intent in [None, Some("nope")] {
        let mut req = c
            .post(&url)
            .header("Origin", origin_header(addr))
            .body(vec![1u8; 16]);
        if let Some(v) = intent {
            req = req.header("x-kb-intent", v);
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "intent {intent:?}");
        let (_s, text) = body_text(resp).await;
        assert_problem(&text, "terminal-intent-required");
    }

    // A token caller is refused by default.
    let resp = c
        .post(&url)
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .body(vec![1u8; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-token-refused");

    // The gate runs before the id check: a refused caller never learns
    // whether the id was valid.
    let resp = c
        .post(files_url(addr, "BAD_ID"))
        .header("x-kb-intent", "terminal")
        .body(vec![1u8; 16])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);

    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "a refused request must never dial the runner"
    );
}
