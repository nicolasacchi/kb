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
use std::sync::Mutex;
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
/// "the gate never touched the runner", plus every request line it
/// receives (the relay payloads the routes hand over).
struct FakeRunner {
    conns: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    socket: std::path::PathBuf,
}

/// The requests the fake runner has received, in order.
fn captured_requests(fake: &FakeRunner) -> Vec<serde_json::Value> {
    fake.requests.lock().unwrap().clone()
}

/// The most recent request the fake runner has received.
fn last_request(fake: &FakeRunner) -> serde_json::Value {
    captured_requests(fake)
        .into_iter()
        .last()
        .expect("the fake runner must have received a request")
}

fn spawn_fake(
    dir: &std::path::Path,
    replies: std::collections::HashMap<String, String>,
) -> FakeRunner {
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let requests: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let counter = conns.clone();
    let capture = requests.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let replies = replies.clone();
            let capture = capture.clone();
            tokio::spawn(async move {
                let mut line = String::new();
                let mut reader = tokio::io::BufReader::new(stream);
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let parsed: serde_json::Value =
                    serde_json::from_str(line.trim()).unwrap_or(serde_json::Value::Null);
                let op = parsed
                    .get("op")
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_default();
                capture.lock().unwrap().push(parsed);
                let reply = replies.get(&op).cloned().unwrap_or_else(|| {
                    "{\"status\":\"err\",\"code\":\"not_found\",\"message\":\"no script\"}"
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
        requests,
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
    // All ten routes with valid Origin+intent → 404 empty body, no
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
    run("multi_line_refused", 422).await;
    run("grid_owned", 409).await;
    run("not_found", 404).await;
    run("bad_request", 400).await;
    run("weird_code", 502).await;

    // Socket absent → 503, body does not contain the socket path.
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("absent.sock");
    let fake = FakeRunner {
        conns: Arc::new(AtomicUsize::new(0)),
        requests: Arc::new(Mutex::new(Vec::new())),
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

#[tokio::test]
async fn paste_32kib_boundary_is_exact() {
    async fn post_text(addr: std::net::SocketAddr, text: &str) -> u16 {
        client()
            .post(format!("{}/api/terms/{TERM_ID}/paste", base_url(addr)))
            .header("Origin", origin_header(addr))
            .header("x-kb-intent", "terminal")
            .header("Content-Type", "application/json")
            .body(format!(
                "{{\"text\":{}}}",
                serde_json::to_string(text).unwrap()
            ))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }
    let mut replies = std::collections::HashMap::new();
    replies.insert("paste".to_string(), ok_reply("{\"pasted\":true}"));
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    // Exactly 32 KiB of ASCII is UNDER the cap (the route relays it).
    assert_eq!(
        post_text(addr, &"a".repeat(32 * 1024)).await,
        200,
        "exactly 32 KiB must not be 413"
    );
    // The relay hands the runner exactly the decoded text.
    assert_eq!(
        last_request(&fake)["text"].as_str().unwrap(),
        "a".repeat(32 * 1024)
    );
    // 32 KiB + 1 byte is over the cap.
    assert_eq!(post_text(addr, &"a".repeat(32 * 1024 + 1)).await, 413);
    // A two-byte char: 16384 of them are exactly 32768 bytes (under
    // the cap, so relayed); that plus one more byte is 32769 bytes
    // but only 16385 chars — the cap counts bytes, not chars.
    assert_eq!(
        post_text(addr, "é").await,
        200,
        "multibyte below the byte cap relays"
    );
    assert_eq!(
        post_text(addr, &"é".repeat(16384)).await,
        200,
        "32768 bytes of multibyte text must not be 413"
    );
    assert_eq!(
        post_text(addr, &"é".repeat(16384) + "a").await,
        413,
        "32769 bytes (16385 chars) is over the byte cap"
    );
    // Only the three accepted pastes reached the runner.
    assert_eq!(captured_requests(&fake).len(), 3);
}

#[tokio::test]
async fn relays_carry_exact_ids_ops_and_payloads() {
    let mut replies = std::collections::HashMap::new();
    replies.insert(
        "launch".to_string(),
        ok_reply("{\"id\":\"abcd1234\",\"harness\":\"omp\"}"),
    );
    replies.insert("paste".to_string(), ok_reply("{\"pasted\":true}"));
    replies.insert("keys".to_string(), ok_reply("{\"sent\":2}"));
    replies.insert("kill".to_string(), ok_reply("{\"killed\":true}"));
    replies.insert(
        "take_size".to_string(),
        ok_reply("{\"cols\":120,\"rows\":40}"),
    );
    replies.insert("enter".to_string(), ok_reply("{\"entered\":true}"));
    replies.insert("read".to_string(), ok_reply("\"synthetic read output\""));
    replies.insert("history".to_string(), ok_reply("[]"));
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), replies);
    let (_tmp, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();

    // GET /terms -> {"op":"list"} and nothing else.
    let resp = c
        .get(format!("{}/api/terms", base_url(addr)))
        .header("Origin", origin_header(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(last_request(&fake), serde_json::json!({"op": "list"}));

    // Launch with no optional fields: the relay supplies the runner's
    // defaults (cwd /, cols 80, rows 24) and every other field null.
    let resp = c
        .post(format!("{}/api/terms", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{\"harness\":\"omp\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"launch","harness":"omp","argv":null,"resume_sid":null,"cwd":"/","cols":80,"rows":24,"env":null,"exit_keys":null})
    );

    // Launch with every field set: each passes through verbatim.
    let resp = c
        .post(format!("{}/api/terms", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{\"harness\":\"claude\",\"resume_sid\":\"sid-x\",\"cwd\":\"/work/proj\",\"cols\":120,\"rows\":40}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"launch","harness":"claude","argv":null,"resume_sid":"sid-x","cwd":"/work/proj","cols":120,"rows":40,"env":null,"exit_keys":null})
    );

    // Paste: id, viewer and text travel verbatim.
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/paste", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{\"text\":\"hello synthetic\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"paste","id":TERM_ID,"viewer":"spa","text":"hello synthetic"})
    );

    // Keys: Named key input shape.
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/keys", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{\"keys\":[\"Enter\",\"ctrl_c\"]}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"keys","id":TERM_ID,"viewer":"spa","input":{"kind":"named","named":["Enter","ctrl_c"]}})
    );

    // Kill: id travels; grace is null.
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/kill", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"kill","id":TERM_ID,"grace_secs":null})
    );

    // Take-size: id, grid, owner and force travel verbatim.
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/take-size", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{\"cols\":120,\"rows\":40}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"take_size","id":TERM_ID,"cols":120,"rows":40,"owner":"kb-term","force":true})
    );

    // Enter: id, viewer, blocked false, from_composer false.
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/enter", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"enter","id":TERM_ID,"viewer":"spa","blocked":false,"from_composer":false})
    );

    // Read / history: id travels.
    let resp = c
        .get(format!("{}/api/terms/{TERM_ID}/read", base_url(addr)))
        .header("Origin", origin_header(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"read","id":TERM_ID})
    );
    let resp = c
        .get(format!("{}/api/terms/{TERM_ID}/history", base_url(addr)))
        .header("Origin", origin_header(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        last_request(&fake),
        serde_json::json!({"op":"history","id":TERM_ID})
    );

    // Ticket: the route's presence probe is a bare list op.
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
    assert_eq!(last_request(&fake), serde_json::json!({"op": "list"}));
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
