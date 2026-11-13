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

#[tokio::test]
async fn absent_origin_is_refused_even_on_get() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), Default::default());
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = c
        .get(format!("{}/api/terms", base_url(addr)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let (_s, text) = body_text(resp).await;
    assert_problem(&text, "terminal-origin-refused");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
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

#[tokio::test]
async fn get_routes_do_not_need_the_intent_header() {
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
    assert_eq!(resp.status().as_u16(), 200);
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
        "{\"harness\":\"omp\",\"cwd\":\"rel/path\"}".to_string(),
        "{\"harness\":\"omp\",\"cwd\":\"/work/proj\",\"resume_sid\":\"bad\\u0000sid\"}".to_string(),
        "{\"harness\":\"omp\",\"cwd\":\"/work/proj\",\"unknown\":1}".to_string(),
    ] {
        let resp = launch(body.clone()).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 400, "body {body}");
    }
    // All six harnesses are accepted.
    for h in ["claude", "codex", "opencode", "grok", "kimi", "omp"] {
        let resp = launch(format!("{{\"harness\":\"{h}\",\"cwd\":\"/work/proj\"}}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 201, "harness {h}");
    }
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

    fn make_writer_for(&self, _meta: &tracing::Metadata<'_>) -> Self::Writer {
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
