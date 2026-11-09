//! The terminal WebSocket relay over the REAL router, with a FAKE unix
//! runner: handler-side gate, byte-for-byte relay, close 1012 on
//! restart, and the low-rate `term.*` SSE kinds (T12).
//!
//! The runner is a `tokio::net::UnixListener` task in the test's
//! tempdir. One-shot RPC ops (`list`, `launch`, `kill`, ...) get keyed
//! scripted replies; the `stream` op gets an ok reply and then the
//! tap envelope splice (`[u32 BE len][u8 kind][idlen, id, frame]`
//! S->C, raw item envelopes C->S). The fake records stream request
//! lines and C->S items, so "the handler never touched the runner"
//! and "browser input reached the upstream unchanged" are observable.
//!
//! The WS client is `tokio-tungstenite`; every socket read carries a
//! hard deadline (a hang must fail the test, not burn the lane).

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::protocol::Message;

const TERM_ID: &str = "abcd1234";
const OTHER_ID: &str = "efgh5678";
const ENV_TEXT: u8 = 0;
const ENV_BINARY: u8 = 1;

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
        "problem body must carry the urn tail {urn_tail}: {text}"
    );
}

// --------------------------------------------------------------------------
// Fixtures (mirrors tests/terms_gate.rs).
// --------------------------------------------------------------------------

fn kb_section(path: std::path::PathBuf) -> kb_core::config::KbSection {
    kb_core::config::KbSection {
        path,
        skip_patterns: Vec::new(),
        ui: kb_core::config::UiSection::default(),
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

// --------------------------------------------------------------------------
// The fake runner.
// --------------------------------------------------------------------------

/// One C->S item the fake read off the stream connection.
type ReceivedItem = (u8, Vec<u8>);

struct StreamFake {
    conns: Arc<AtomicUsize>,
    socket: std::path::PathBuf,
    stream_reqs: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    received: Arc<std::sync::Mutex<Vec<ReceivedItem>>>,
}

/// The tap's S->C envelope: `[u32 BE len][u8 kind][idlen, id, frame]`,
/// with `len` counting the kind byte + body. Viewer id "v".
fn s2c_envelope(kind: u8, frame: &[u8]) -> Vec<u8> {
    let viewer = b"v";
    let mut body = Vec::new();
    body.push(viewer.len() as u8);
    body.extend_from_slice(viewer);
    body.extend_from_slice(frame);
    let mut out = Vec::new();
    out.extend_from_slice(&((1 + body.len()) as u32).to_be_bytes());
    out.push(kind);
    out.extend_from_slice(&body);
    out
}

/// A binary frame: 8-byte BE offset header + payload.
fn binary_frame(offset: u64, payload: &[u8]) -> Vec<u8> {
    let mut v = offset.to_be_bytes().to_vec();
    v.extend_from_slice(payload);
    v
}

fn spawn_stream_fake(
    dir: &std::path::Path,
    replies: HashMap<String, String>,
    script: Vec<Vec<u8>>,
    close_after: bool,
) -> StreamFake {
    spawn_stream_fake_mode(dir, replies, script, close_after, None)
}

/// Like [`spawn_stream_fake`], but `stream_err = Some(code)` makes the
/// fake answer the `stream` op with an error reply (code `code`)
/// instead of an ok reply — the stream-error mapping pin.
fn spawn_stream_fake_mode(
    dir: &std::path::Path,
    replies: HashMap<String, String>,
    script: Vec<Vec<u8>>,
    close_after: bool,
    stream_err: Option<&'static str>,
) -> StreamFake {
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let stream_reqs = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (counter, reqs2, received2) = (conns.clone(), stream_reqs.clone(), received.clone());
    let script = Arc::new(script);
    let replies = Arc::new(replies);
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let (reqs3, received3) = (reqs2.clone(), received2.clone());
            let script3 = script.clone();
            let replies3 = replies.clone();
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let req: serde_json::Value =
                    serde_json::from_str(line.trim()).unwrap_or(serde_json::Value::Null);
                let op = req
                    .get("op")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if op == "stream" {
                    reqs3.lock().unwrap().push(req);
                    let mut stream = reader.into_inner();
                    if let Some(code) = stream_err {
                        let err = format!(
                            "{{\"status\":\"err\",\"code\":\"{code}\",\"message\":\"scripted stream error\"}}\n"
                        );
                        let _ = stream.write_all(err.as_bytes()).await;
                        return;
                    }
                    if stream
                        .write_all(b"{\"status\":\"ok\",\"data\":{}}\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    for env in script3.iter() {
                        if stream.write_all(env).await.is_err() {
                            return;
                        }
                    }
                    if close_after {
                        return;
                    }
                    // Hold the connection open, recording C->S items.
                    loop {
                        let mut hdr = [0u8; 4];
                        if stream.read_exact(&mut hdr).await.is_err() {
                            return;
                        }
                        let len = u32::from_be_bytes(hdr) as usize;
                        if len == 0 || len > 1024 * 1024 + 512 {
                            return;
                        }
                        let mut buf = vec![0u8; len];
                        if stream.read_exact(&mut buf).await.is_err() {
                            return;
                        }
                        let kind = buf[0];
                        let body = buf[1..].to_vec();
                        received3.lock().unwrap().push((kind, body));
                    }
                } else {
                    let reply = replies3.get(&op).cloned().unwrap_or_else(|| {
                        "{\"status\":\"err\",\"code\":\"not_found\",\"message\":\"no script\"}"
                            .to_string()
                    });
                    let mut stream = reader.into_inner();
                    let _ = stream.write_all(reply.as_bytes()).await;
                    let _ = stream.write_all(b"\n").await;
                }
            });
        }
    });
    StreamFake {
        conns,
        socket: sock,
        stream_reqs,
        received,
    }
}

// --------------------------------------------------------------------------
// Boot.
// --------------------------------------------------------------------------

async fn boot(
    terminal: kb_core::config::TerminalSection,
    fake: &StreamFake,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let (tmp, addr, _handle) = boot_with_handle(terminal, fake).await;
    (tmp, addr)
}

async fn boot_with_handle(
    terminal: kb_core::config::TerminalSection,
    fake: &StreamFake,
) -> (
    tempfile::TempDir,
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<(), anyhow::Error>>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();

    use kb_core::config::{
        DaemonSection, DefaultsSection, KbConfig, KbName, TerminalSection, UiSection,
    };
    let _ = TerminalSection::default();
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: std::collections::BTreeMap<KbName, kb_core::config::KbSection> =
        std::collections::BTreeMap::new();
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

    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, handle) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    (tmp, addr, handle)
}

fn enabled_terminal() -> kb_core::config::TerminalSection {
    kb_core::config::TerminalSection {
        enabled: Some(true),
        ..Default::default()
    }
}

/// The bearer token this file's token-caller test installs into the
/// daemon's token file (synthetic; never a real credential).
const FIXTURE_TOKEN: &str = "test-token-fixture-deadbeef";

/// Like [`boot`], but the daemon also accepts `FIXTURE_TOKEN` (the
/// token file is written at the daemon's config path before serve),
/// so a bearer-authenticated caller gets `IdentitySource::Token`.
async fn boot_with_token(
    terminal: kb_core::config::TerminalSection,
    fake: &StreamFake,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();

    use kb_core::config::{DaemonSection, DefaultsSection, KbConfig, KbName, UiSection};
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: std::collections::BTreeMap<KbName, kb_core::config::KbSection> =
        std::collections::BTreeMap::new();
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

    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), daemon_name);
    std::fs::create_dir_all(&paths.config).unwrap();
    std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    let (addr, _handle) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    (tmp, addr)
}

// --------------------------------------------------------------------------
// HTTP helpers.
// --------------------------------------------------------------------------

/// A plain-HTTP "upgrade attempt": carries the WS handshake headers
/// (so axum's `WebSocketUpgrade` extractor accepts and the HANDLER
/// runs), but reqwest never performs the WS handshake — the handler's
/// plain HTTP refusals come back as responses.
async fn ws_http_get(
    addr: std::net::SocketAddr,
    id: &str,
    ticket: &str,
    from: Option<&str>,
    origin: Option<&str>,
    remote_user: Option<&str>,
) -> (u16, String) {
    let mut url = format!("{}/api/terms/{id}/ws?ticket={ticket}", base_url(addr));
    if let Some(f) = from {
        url.push_str("&from=");
        url.push_str(f);
    }
    let mut req = client()
        .get(url)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Sec-WebSocket-Version", "13");
    if let Some(o) = origin {
        req = req.header("Origin", o);
    }
    if let Some(u) = remote_user {
        req = req.header("Remote-User", u);
    }
    let resp = req.send().await.unwrap();
    body_text(resp).await
}

async fn mint_ticket(addr: std::net::SocketAddr, id: &str, remote_user: Option<&str>) -> String {
    let mut req = client()
        .post(format!("{}/api/terms/{id}/ticket", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{}");
    if let Some(u) = remote_user {
        req = req.header("Remote-User", u);
    }
    let resp = req.send().await.unwrap();
    let (status, text) = body_text(resp).await;
    assert_eq!(status, 200, "ticket mint must succeed: {text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["ticket"].as_str().unwrap().to_string()
}

// --------------------------------------------------------------------------
// WS helpers.
// --------------------------------------------------------------------------

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
#[derive(Debug)]

enum WsConnect {
    Ok(WsStream),
    Http(u16),
    Other,
}

#[derive(Debug)]
enum WsOutcome {
    Data(Message),
    Close(u16, String),
    Ended,
    TimedOut,
}

async fn ws_connect(
    addr: std::net::SocketAddr,
    id: &str,
    ticket: &str,
    from: Option<&str>,
    origin: &str,
    remote_user: Option<&str>,
) -> WsConnect {
    let mut url = format!("ws://{addr}/api/terms/{id}/ws?ticket={ticket}");
    if let Some(f) = from {
        url.push_str("&from=");
        url.push_str(f);
    }
    let mut builder = axum::http::Request::builder()
        .uri(url)
        .header("Origin", origin)
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Sec-WebSocket-Version", "13");
    if let Some(u) = remote_user {
        builder = builder.header("Remote-User", u);
    }
    // Tungstenite fills in `Upgrade` / `Connection` itself; the key and
    // version above pin the handshake shape axum expects.
    let req = builder.body(()).unwrap();
    match tokio_tungstenite::connect_async_with_config(req, None, false).await {
        Ok((ws, _resp)) => WsConnect::Ok(ws),
        Err(tokio_tungstenite::tungstenite::error::Error::Http(resp)) => {
            WsConnect::Http(resp.status().as_u16())
        }
        Err(_) => WsConnect::Other,
    }
}

async fn ws_next(ws: &mut WsStream, dur: Duration) -> WsOutcome {
    match tokio::time::timeout(dur, ws.next()).await {
        Err(_) => WsOutcome::TimedOut,
        Ok(None) => WsOutcome::Ended,
        Ok(Some(Ok(msg))) => match msg {
            Message::Close(frame) => {
                let (code, reason) = match frame {
                    Some(f) => (u16::from(f.code), f.reason.to_string()),
                    None => (0, String::new()),
                };
                WsOutcome::Close(code, reason)
            }
            m => WsOutcome::Data(m),
        },
        Ok(Some(Err(tokio_tungstenite::tungstenite::error::Error::ConnectionClosed))) => {
            WsOutcome::Ended
        }
        Ok(Some(Err(_))) => WsOutcome::Ended,
    }
}

/// Read `n` data messages (skipping pings), then the close, with a
/// per-message deadline. Returns (data, Option<(code, reason)>).
async fn ws_read_n(ws: &mut WsStream, n: usize) -> (Vec<Message>, Option<(u16, String)>) {
    let mut data = Vec::new();
    let mut close = None;
    while data.len() < n {
        match ws_next(ws, Duration::from_secs(5)).await {
            WsOutcome::Data(m) => data.push(m),
            WsOutcome::Close(code, reason) => {
                close = Some((code, reason));
                break;
            }
            WsOutcome::Ended | WsOutcome::TimedOut => break,
        }
    }
    if data.len() >= n && close.is_none() {
        match ws_next(ws, Duration::from_secs(5)).await {
            WsOutcome::Close(code, reason) => close = Some((code, reason)),
            _ => {}
        }
    }
    (data, close)
}

// --------------------------------------------------------------------------
// SSE helper.
// --------------------------------------------------------------------------

struct SseTerm {
    event: String,
    payload: serde_json::Value,
}

async fn sse_term_events(addr: std::net::SocketAddr, wait: Duration) -> Vec<SseTerm> {
    let mut resp = client()
        .get(format!(
            "{}/api/events?types=term.created,term.ended,term.state",
            base_url(addr)
        ))
        .header("last-event-id", "0")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "events stream must open");
    let mut out = Vec::new();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let remain = deadline - tokio::time::Instant::now();
        let chunk = match tokio::time::timeout(remain, resp.chunk()).await {
            Ok(Ok(Some(bytes))) => bytes,
            _ => break,
        };
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(pos) = buf.find("\n\n") {
            let frame: String = buf.drain(..pos + 2).collect();
            let mut event = String::new();
            let mut data = String::new();
            for line in frame.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().to_string();
                } else if let Some(v) = line.strip_prefix("data:") {
                    data = v.trim().to_string();
                }
            }
            if event.is_empty() || data.is_empty() {
                continue;
            }
            let body: serde_json::Value =
                serde_json::from_str(&data).unwrap_or(serde_json::json!({}));
            let payload = body
                .get("payload")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            out.push(SseTerm { event, payload });
        }
    }
    out
}

// --------------------------------------------------------------------------
// Spec 8, test 1.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_disabled_is_bodiless_404() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_stream_fake(tmp.path(), HashMap::new(), vec![], true);
    let (tmp, addr) = boot(
        kb_core::config::TerminalSection {
            enabled: Some(false),
            ..Default::default()
        },
        &fake,
    )
    .await;
    let _ = tmp;
    let (status, text) = ws_http_get(
        addr,
        TERM_ID,
        "deadbeefdeadbeefdeadbeefdeadbeef",
        None,
        Some(&origin_header(addr)),
        None,
    )
    .await;
    assert_eq!(status, 404, "disabled terminal must bodiless-404: {text}");
    assert_eq!(text, "", "the disabled 404 carries no body: {text:?}");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0, "0 runner connections");
}

// --------------------------------------------------------------------------
// Spec 8, test 2 (adapted to the merged check_origin: same-host Origin
// is accepted alongside the exact parent origin).
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_requires_exact_origin() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        true,
    );
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    // Absent, foreign, and artifact-subdomain Origins are all refused.
    for origin in [
        None,
        Some("http://evil.example"),
        Some("http://x.artifacts.localhost"),
    ] {
        let (status, text) = ws_http_get(addr, TERM_ID, &ticket, None, origin, None).await;
        assert_eq!(status, 403, "origin {origin:?}");
        assert_problem(&text, "terminal-origin-refused");
    }
    // None of the refusals consumed the ticket: the same ticket upgrades.
    match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(mut ws) => {
            let (data, _) = ws_read_n(&mut ws, 1).await;
            assert_eq!(data.len(), 1, "same-host Origin must upgrade");
        }
        other => panic!("same-host Origin must upgrade, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// Spec 8, test 3.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_loopback_does_not_bypass() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_stream_fake(tmp.path(), HashMap::new(), vec![], true);
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    // A loopback caller with no VALID ticket gets the ticket 403 —
    // the loopback bypass never applies to this gate (there is no
    // ticket-less caller on the WS path at all: the ticket IS the auth).
    let (status, text) = ws_http_get(
        addr,
        TERM_ID,
        "deadbeefdeadbeefdeadbeefdeadbeef",
        None,
        Some(&origin_header(addr)),
        None,
    )
    .await;
    assert_eq!(status, 403, "no valid ticket on loopback must refuse");
    assert_problem(&text, "terminal-ticket-invalid");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0, "0 runner connections");
}

// --------------------------------------------------------------------------
// Spec 8, test 4.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_ticket_single_use() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        true,
    );
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(mut ws) => {
            let (data, _) = ws_read_n(&mut ws, 1).await;
            assert_eq!(data.len(), 1);
            drop(ws);
        }
        other => panic!("first connect must succeed, got {other:?}"),
    }
    let (status, text) = ws_http_get(
        addr,
        TERM_ID,
        &ticket,
        None,
        Some(&origin_header(addr)),
        None,
    )
    .await;
    assert_eq!(status, 403, "replayed ticket must refuse");
    assert_problem(&text, "terminal-ticket-invalid");
}

// --------------------------------------------------------------------------
// Spec 8, test 5.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_ticket_bound_to_session_and_identity() {
    // Wrong session (ticket minted for A used on B) is indistinguishable
    // from every other ticket failure — including expired.
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(tmp.path(), replies, vec![], true);
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;

    // Unknown-ticket reference body.
    let (_s, unknown_body) = ws_http_get(
        addr,
        TERM_ID,
        "ffffffffffffffffffffffffffffffff",
        None,
        Some(&origin_header(addr)),
        None,
    )
    .await;

    // Wrong session: mint for TERM_ID, upgrade on OTHER_ID.
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let (status, wrong_session_body) = ws_http_get(
        addr,
        OTHER_ID,
        &ticket,
        None,
        Some(&origin_header(addr)),
        None,
    )
    .await;
    assert_eq!(status, 403, "wrong session must refuse");
    assert_problem(&wrong_session_body, "terminal-ticket-invalid");

    // Wrong identity: mint as alice (Remote-User), upgrade as bob.
    let ticket_alice = mint_ticket(addr, TERM_ID, Some("alice")).await;
    let (status, wrong_user_body) = ws_http_get(
        addr,
        TERM_ID,
        &ticket_alice,
        None,
        Some(&origin_header(addr)),
        Some("bob"),
    )
    .await;
    assert_eq!(status, 403, "wrong identity must refuse");
    assert_problem(&wrong_user_body, "terminal-ticket-invalid");

    // All ticket failures share one byte-identical body (no oracle).
    assert_eq!(
        unknown_body, wrong_session_body,
        "wrong-session body must match unknown"
    );
    assert_eq!(
        unknown_body, wrong_user_body,
        "wrong-user body must match unknown"
    );

    // Expired: a 2-second TTL ticket waited out (a 25%+ margin: the
    // old 1.2 s / 1 s pairing flaked ~20% live) matches the same
    // body (separate boot: the TTL is a config knob).
    let tmp2 = tempfile::tempdir().unwrap();
    let mut replies2 = HashMap::new();
    replies2.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake2 = spawn_stream_fake(tmp2.path(), replies2, vec![], true);
    let (tmp2, addr2) = boot(
        kb_core::config::TerminalSection {
            enabled: Some(true),
            ticket_ttl_secs: Some(2),
            ..Default::default()
        },
        &fake2,
    )
    .await;
    let _ = tmp2;
    let expiring = mint_ticket(addr2, TERM_ID, None).await;
    // Poll until the GET is refused (25% margin trips if the TTL
    // ever becomes unenforceable), up to a 5 s deadline: a test
    // that never expires must fail here, not sleep blindly.
    let (status, expired_body) = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let (status, expired_body) = ws_http_get(
                addr2,
                TERM_ID,
                &expiring,
                None,
                Some(&origin_header(addr2)),
                None,
            )
            .await;
            if status == 403 {
                break (status, expired_body);
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "an expired ticket must refuse within 5 s"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    assert_eq!(
        unknown_body, expired_body,
        "expired body must match unknown (no oracle)"
    );
}

// --------------------------------------------------------------------------
// Spec 8, test 6.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_relays_frames_verbatim_and_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let script = vec![
        s2c_envelope(ENV_TEXT, br#"{"t":"hello","proto":"kb-term/1"}"#),
        s2c_envelope(ENV_BINARY, &binary_frame(0, b"abc")),
        s2c_envelope(
            ENV_TEXT,
            b"{\"t\":\"resize\",\"seq\":1,\"offset\":3,\"cols\":120,\"rows\":40}",
        ),
        s2c_envelope(ENV_BINARY, &binary_frame(3, b"def")),
        s2c_envelope(ENV_TEXT, br#"{"t":"exit","seq":2,"code":0,"offset":6}"#),
    ];
    let fake = spawn_stream_fake(tmp.path(), replies, script, true);
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let mut ws = match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(w) => w,
        other => panic!("connect must succeed, got {other:?}"),
    };
    let (frames, close) = ws_read_n(&mut ws, 5).await;
    assert_eq!(
        frames.len(),
        5,
        "five upstream frames, verbatim: {frames:?}"
    );
    match &frames[0] {
        Message::Text(t) => assert_eq!(t.as_str(), r#"{"t":"hello","proto":"kb-term/1"}"#),
        other => panic!("frame 0 must be hello text, got {other:?}"),
    }
    match &frames[1] {
        Message::Binary(b) => assert_eq!(b.as_ref(), &binary_frame(0, b"abc")[..]),
        other => panic!("frame 1 must be the offset-0 binary, got {other:?}"),
    }
    match &frames[2] {
        Message::Text(t) => assert!(t.as_str().contains("resize")),
        other => panic!("frame 2 must be the resize text, got {other:?}"),
    }
    match &frames[3] {
        Message::Binary(b) => assert_eq!(b.as_ref(), &binary_frame(3, b"def")[..]),
        other => panic!("frame 3 must be the offset-3 binary, got {other:?}"),
    }
    match &frames[4] {
        Message::Text(t) => assert_eq!(t.as_str(), r#"{"t":"exit","seq":2,"code":0,"offset":6}"#),
        other => panic!("frame 4 must be the exit text, got {other:?}"),
    }
    assert_eq!(
        close,
        Some((1000, "normal".to_string())),
        "exit flushed, then close 1000"
    );
}

// --------------------------------------------------------------------------
// Spec 8, test 7.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_input_reaches_upstream_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        false,
    );
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let mut ws = match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(w) => w,
        other => panic!("connect must succeed, got {other:?}"),
    };
    // Let the hello arrive (relay is up), then type.
    let _ = ws_read_n(&mut ws, 1).await;
    let key_bytes: [u8; 7] = [0x1b, 0x5b, 0x31, 0x33, 0x3b, 0x32, 0x75];
    ws.send(Message::binary(key_bytes.to_vec())).await.unwrap();
    ws.send(Message::text(r#"{"t":"resumefrom","offset":42}"#))
        .await
        .unwrap();
    // Deadline-bounded poll (no fixed sleep): both forwards must
    // arrive within 5 s, in order. The fake records raw C->S bodies,
    // which carry the tap's S2C viewer-id prefix (kb-server wraps
    // every item); the frame bytes inside are byte-for-byte.
    let strip_s2c = |body: &[u8]| -> Vec<u8> {
        let id_len = body[0] as usize;
        assert!(body.len() >= 1 + id_len, "wrapped body must hold its id");
        body[1 + id_len..].to_vec()
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let received = loop {
        let got = fake.received.lock().unwrap().clone();
        if got.len() >= 2 {
            break got;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "browser input must reach the upstream within 5 s: {got:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    drop(ws);
    let frames: Vec<(u8, Vec<u8>)> = received
        .into_iter()
        .map(|(kind, body)| (kind, strip_s2c(&body)))
        .collect();
    assert_eq!(
        frames,
        vec![
            (ENV_BINARY, key_bytes.to_vec()),
            (ENV_TEXT, br#"{"t":"resumefrom","offset":42}"#.to_vec()),
        ],
        "browser input must reach the upstream byte-for-byte, in order: {frames:?}"
    );
}

// --------------------------------------------------------------------------
// Spec 8, test 8.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_from_param_forwarded() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        false,
    );
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;

    // ?from=1234 forwards as ResumeInfo offset.
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let mut ws = match ws_connect(
        addr,
        TERM_ID,
        &ticket,
        Some("1234"),
        &origin_header(addr),
        None,
    )
    .await
    {
        WsConnect::Ok(w) => w,
        other => panic!("connect with ?from=1234 must succeed, got {other:?}"),
    };
    let _ = ws_read_n(&mut ws, 1).await;
    let reqs = fake.stream_reqs.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].get("from"),
        Some(&serde_json::json!(1234)),
        "?from=1234 must forward: {}",
        reqs[0]
    );
    drop(ws);

    // No ?from: full join -> from null/absent.
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let mut ws = match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(w) => w,
        other => panic!("connect without ?from must succeed, got {other:?}"),
    };
    let _ = ws_read_n(&mut ws, 1).await;
    let reqs = fake.stream_reqs.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2);
    assert!(
        reqs[1].get("from").is_none() || reqs[1]["from"].is_null(),
        "absent ?from must be null/absent: {}",
        reqs[1]
    );
    drop(ws);

    // ?from=abc -> 400, no upgrade, no stream, and the ticket survives
    // (from is parsed before the ticket is consumed).
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let conns_before = fake.conns.load(Ordering::SeqCst);
    let (status, _text) = ws_http_get(
        addr,
        TERM_ID,
        &ticket,
        Some("abc"),
        Some(&origin_header(addr)),
        None,
    )
    .await;
    assert_eq!(
        status, 400,
        "garbage ?from must 400 before any ticket/runner work"
    );
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        conns_before,
        "a malformed ?from must not open the upstream"
    );
    match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(mut ws) => {
            let _ = ws_read_n(&mut ws, 1).await;
        }
        other => panic!("the malformed-?from ticket must survive, got {other:?}"),
    }
}

// --------------------------------------------------------------------------
// Spec 8, test 11.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_socket_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        false,
    );
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;

    // 16 live sockets fill the cap; the 17th upgrade refuses 503 and
    // its ticket survives; freeing one slot lets it through.
    let mut sockets = Vec::new();
    for _ in 0..16 {
        let ticket = mint_ticket(addr, TERM_ID, None).await;
        match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
            WsConnect::Ok(ws) => sockets.push(ws),
            other => panic!("each of the first 16 sockets must connect, got {other:?}"),
        }
    }
    let spare = mint_ticket(addr, TERM_ID, None).await;
    match ws_connect(addr, TERM_ID, &spare, None, &origin_header(addr), None).await {
        WsConnect::Http(503) => {}
        other => panic!("the 17th socket must cap at 503, got {other:?}"),
    }
    // Free a slot, then reuse the spare ticket (it was not consumed).
    drop(sockets.remove(0));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match ws_connect(addr, TERM_ID, &spare, None, &origin_header(addr), None).await {
            WsConnect::Ok(mut ws) => {
                let _ = ws_read_n(&mut ws, 1).await;
                break;
            }
            WsConnect::Http(503) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            other => panic!("the spare ticket must upgrade once a slot frees, got {other:?}"),
        }
    }
}

// --------------------------------------------------------------------------
// Spec 8, test 12 (spec 7's proof: the shutdown-watch arm alone drives
// the drain — lib.rs stays untouched).
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_shutdown_sends_close_1012_and_drain_finishes_fast() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        false,
    );
    let (tmp, addr, handle) = boot_with_handle(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let mut ws = match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(w) => w,
        other => panic!("connect must succeed, got {other:?}"),
    };
    let (data, _) = ws_read_n(&mut ws, 1).await;
    assert_eq!(data.len(), 1, "relay must be live before shutdown");

    let resp = client()
        .post(format!("{}/api/shutdown", base_url(addr)))
        .header("Origin", origin_header(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 202, "shutdown must acknowledge");

    match ws_next(&mut ws, Duration::from_secs(5)).await {
        WsOutcome::Close(code, reason) => {
            assert_eq!(
                code, 1012,
                "shutdown close must be 1012, got {code} ({reason})"
            );
            assert_eq!(reason, "service restart", "1012 reason");
        }
        other => panic!("the client must receive Close(1012), got {other:?}"),
    }
    // The server's serve JoinHandle must complete well under
    // GRACEFUL_DRAIN_SECS (5s) — the shutdown arm alone suffices.
    let start = std::time::Instant::now();
    let joined = tokio::time::timeout(Duration::from_secs(3), handle).await;
    assert!(
        joined.is_ok(),
        "the serve JoinHandle must finish within 3s of shutdown (spec 7 proof)"
    );
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "drain must be fast"
    );
}

// --------------------------------------------------------------------------
// Spec 8, test 13.
// --------------------------------------------------------------------------

/// A capturing `MakeWriter` for the log test: shared byte buffer.
#[derive(Clone)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> From<&'a Capture> for Capture {
    fn from(v: &'a Capture) -> Self {
        v.clone()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn ws_does_not_log_ticket() {
    // The ticket rides the query string (browsers cannot set headers
    // on a WS handshake) and is consumed at the upgrade, so a logged
    // copy is dead; on top of that the handler never logs it, AND
    // tower-http's TraceLayer span no longer carries the URI query
    // (router.rs installs a query-less span: queries hold
    // credentials). Both halves are pinned here over
    // `kb_server::terminal` + `tower_http` at TRACE.
    //
    // The subscriber is process-wide (tracing has only one global),
    // so the liveness assertion is scoped to this test's own term id
    // (`term=abcd1234` on the upgrade line), not just "some upgrade".
    let buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let writer = Capture(buf.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("kb_server::terminal=trace,tower_http=trace")
        .with_writer(writer)
        .with_ansi(false)
        .without_time()
        .finish();
    // Only this test installs a global subscriber; other tests in the
    // binary do not read it.
    let _ = tracing::subscriber::set_global_default(subscriber);

    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        false,
    );
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let mut ws = match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(w) => w,
        other => panic!("connect must succeed, got {other:?}"),
    };
    let _ = ws_read_n(&mut ws, 1).await;
    drop(ws);
    // Poll (no fixed sleep) until THIS test's upgrade line appears
    // — the capture is live and scoped to our term id — then pin the
    // ticket's absence over the full captured buffer.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let captured = loop {
        let captured = String::from_utf8_lossy(&buf.lock().unwrap().clone()).to_string();
        if captured.contains("terminal ws upgraded") && captured.contains("term=abcd1234") {
            break captured;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the capture must be live (this test's upgrade line): {captured}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        !captured.contains(&ticket),
        "the ticket must never appear in terminal/tower_http logs: {captured}"
    );
}

// --------------------------------------------------------------------------
// Spec 8, test 14.
// --------------------------------------------------------------------------

#[tokio::test]
async fn term_events_low_rate_and_ended_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let mut script = Vec::new();
    for i in 0..1000u64 {
        script.push(s2c_envelope(ENV_BINARY, &binary_frame(i * 4, b"xxxx")));
    }
    script.push(s2c_envelope(
        ENV_TEXT,
        br#"{"t":"exit","seq":1,"code":0,"offset":4000}"#,
    ));
    script.push(s2c_envelope(
        ENV_TEXT,
        br#"{"t":"exit","seq":2,"code":0,"offset":4000}"#,
    ));
    let fake = spawn_stream_fake(tmp.path(), replies, script, true);
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let mut ws = match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(w) => w,
        other => panic!("connect must succeed, got {other:?}"),
    };
    // Drain all 1002 frames; the stream then closes 1000 after exit.
    let (frames, close) = ws_read_n(&mut ws, 1002).await;
    assert_eq!(frames.len(), 1002, "all scripted frames relayed");
    assert_eq!(
        close,
        Some((1000, "normal".to_string())),
        "ends 1000 after exit"
    );
    drop(ws);

    let events = sse_term_events(addr, Duration::from_millis(1500)).await;
    let created: Vec<_> = events
        .iter()
        .filter(|e| e.event == "term.created")
        .collect();
    let ended: Vec<_> = events.iter().filter(|e| e.event == "term.ended").collect();
    let state: Vec<_> = events.iter().filter(|e| e.event == "term.state").collect();
    assert!(
        created.is_empty(),
        "1000 data frames + exits: no term.created: {created:?}"
    );
    // 1000 binary data frames produced ZERO state events (low rate);
    // the exit produced exactly one `ended` state alongside the
    // single `term.ended` (spec 5).
    assert_eq!(
        state.len(),
        1,
        "exactly one ended term.state despite two exit frames: {events:?}"
    );
    assert_eq!(state[0].payload["id"], TERM_ID, "state payload id");
    assert_eq!(state[0].payload["state"], "ended", "the ended state");
    assert_eq!(
        ended.len(),
        1,
        "exactly one term.ended despite two exit frames: {events:?}"
    );
    assert_eq!(ended[0].payload["id"], TERM_ID, "ended payload id");
}

// --------------------------------------------------------------------------
// Spec 8, test 15.
// --------------------------------------------------------------------------

#[tokio::test]
async fn terminal_emits_created_and_ended_once() {
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert(
        "launch".to_string(),
        "{\"status\":\"ok\",\"data\":{\"id\":\"abcd1234\",\"harness\":\"omp\",\"cols\":100,\"rows\":30}}"
            .to_string(),
    );
    replies.insert(
        "kill".to_string(),
        "{\"status\":\"ok\",\"data\":{\"id\":\"abcd1234\",\"exit_code\":0}}".to_string(),
    );
    let fake = spawn_stream_fake(tmp.path(), replies, vec![], true);
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;

    // The launch BODY asks for 111x22, but the runner confirms
    // 100x30: the created event must carry the RUNNER's dimensions
    // (reply first, body second, defaults last).
    let c = client();
    let resp = c
        .post(format!("{}/api/terms", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body(r#"{"harness":"omp","cols":111,"rows":22}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        201,
        "launch confirmed by the runner"
    );
    let resp = c
        .post(format!("{}/api/terms/{TERM_ID}/kill", base_url(addr)))
        .header("Origin", origin_header(addr))
        .header("x-kb-intent", "terminal")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "kill confirmed by the runner");

    let events = sse_term_events(addr, Duration::from_millis(1500)).await;
    let created: Vec<_> = events
        .iter()
        .filter(|e| e.event == "term.created")
        .collect();
    let ended: Vec<_> = events.iter().filter(|e| e.event == "term.ended").collect();
    let state: Vec<_> = events.iter().filter(|e| e.event == "term.state").collect();
    assert_eq!(created.len(), 1, "exactly one term.created: {events:?}");
    let keys: std::collections::HashSet<&str> = created[0]
        .payload
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert_eq!(
        keys,
        ["id", "harness", "cols", "rows"].into_iter().collect(),
        "created payload keys: {}",
        created[0].payload
    );
    assert_eq!(created[0].payload["id"], TERM_ID);
    assert_eq!(created[0].payload["harness"], "omp");
    assert_eq!(
        created[0].payload["cols"], 100,
        "the runner's confirmed cols, not the body's 111: {}",
        created[0].payload
    );
    assert_eq!(
        created[0].payload["rows"], 30,
        "the runner's confirmed rows, not the body's 22: {}",
        created[0].payload
    );
    assert_eq!(ended.len(), 1, "exactly one term.ended: {events:?}");
    assert_eq!(ended[0].payload["id"], TERM_ID);
    assert_eq!(ended[0].payload["exit_code"], 0);
    assert_eq!(
        state.len(),
        1,
        "the kill must emit the ended state alongside term.ended: {events:?}"
    );
    assert_eq!(state[0].payload["id"], TERM_ID);
    assert_eq!(state[0].payload["state"], "ended");
}
// --------------------------------------------------------------------------
// Review round 1: gate negatives, stream-error mapping, shutdown twin.
// --------------------------------------------------------------------------

#[tokio::test]
async fn ws_caller_class_token_refused_and_ticket_survives() {
    // A bearer (Token) caller is refused at the caller-class step
    // (before the ticket is touched), and the refusal does NOT
    // consume the ticket: the same ticket upgrades for the SPA
    // caller.
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        false,
    );
    let (tmp, addr) = boot_with_token(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    // Same upgrade attempt with the bearer token attached.
    let url = format!("{}/api/terms/{TERM_ID}/ws?ticket={ticket}", base_url(addr));
    let resp = client()
        .get(url)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("Sec-WebSocket-Version", "13")
        .header("Origin", origin_header(addr))
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    let (status, text) = body_text(resp).await;
    assert_eq!(status, 403, "token caller must refuse");
    assert_problem(&text, "terminal-token-refused");
    match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(mut ws) => {
            let (data, _) = ws_read_n(&mut ws, 1).await;
            assert_eq!(data.len(), 1, "the ticket must survive the token refusal");
        }
        other => panic!("the ticket must survive the token refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn ws_gate_order_negatives() {
    // Order pins: (a) id shape (400) runs before the ticket, so a
    // malformed id with a valid ticket is 400 and the ticket
    // survives; (b) `from` parse (400) runs before the cap (503),
    // so a malformed `from` at the cap is 400, never 503, and the
    // ticket survives that too.
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        false,
    );
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;

    let ticket = mint_ticket(addr, TERM_ID, None).await;
    // (a) 7-char and 13-char ids: 400 invalid-id, ticket untouched.
    for bad_id in ["abcd123", "abcd1234wxyz1"] {
        let (status, text) = ws_http_get(
            addr,
            bad_id,
            &ticket,
            None,
            Some(&origin_header(addr)),
            None,
        )
        .await;
        assert_eq!(status, 400, "id {bad_id} must 400 before the ticket");
        assert_problem(&text, "bad-request");
    }
    match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(mut ws) => {
            let (data, _) = ws_read_n(&mut ws, 1).await;
            assert_eq!(data.len(), 1, "the ticket must survive the id negatives");
            drop(ws);
        }
        other => panic!("the ticket must survive the id negatives, got {other:?}"),
    }

    // (b) fill the cap, then a garbage `from` is 400 (from-parse
    // before cap), and the ticket still survives.
    let mut sockets = Vec::new();
    for _ in 0..15 {
        let ticket = mint_ticket(addr, TERM_ID, None).await;
        match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
            WsConnect::Ok(ws) => sockets.push(ws),
            other => panic!("cap fill must connect, got {other:?}"),
        }
    }
    // One socket from (a) was dropped above (its slot frees
    // asynchronously); fill whichever slot remains before minting.
    let spare = mint_ticket(addr, TERM_ID, None).await;
    match ws_connect(addr, TERM_ID, &spare, None, &origin_header(addr), None).await {
        WsConnect::Ok(ws) => sockets.push(ws),
        other => panic!("spare connect must fill the last slot, got {other:?}"),
    }
    // The cap is full: a malformed `from` must 400, not 503.
    let capped = mint_ticket(addr, TERM_ID, None).await;
    let (status, text) = ws_http_get(
        addr,
        TERM_ID,
        &capped,
        Some("abc"),
        Some(&origin_header(addr)),
        None,
    )
    .await;
    assert_eq!(
        status, 400,
        "a malformed `from` at the cap must 400 (from-parse before cap): {text}"
    );
    // Free one slot and reuse the capped ticket (no `from`).
    drop(sockets.remove(0));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match ws_connect(addr, TERM_ID, &capped, None, &origin_header(addr), None).await {
            WsConnect::Ok(_) => break,
            WsConnect::Http(503) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            other => panic!("the capped ticket must upgrade once a slot frees, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn ws_stream_errors_map_and_upstream_failure_consumes_ticket() {
    // (a) Runner says not-found for the stream op -> 404; the ticket
    // is consumed (auth runs before the upstream dial, spec 1: an
    // unauthenticated probe cannot open runner streams).
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake_mode(tmp.path(), replies, vec![], true, Some("not-found"));
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let (status, text) = ws_http_get(
        addr,
        TERM_ID,
        &ticket,
        None,
        Some(&origin_header(addr)),
        None,
    )
    .await;
    assert_eq!(status, 404, "runner not-found must 404: {text}");
    assert_problem(&text, "terminal-not-found");
    let (status, text) = ws_http_get(
        addr,
        TERM_ID,
        &ticket,
        None,
        Some(&origin_header(addr)),
        None,
    )
    .await;
    assert_eq!(
        status, 403,
        "a stream-open failure must consume the ticket (spec 1 order): {text}"
    );

    // (b) Runner unreachable (socket file gone) -> 502 (spec 1.6:
    // runner unreachable is 502, not the REST ops' 503).
    let tmp2 = tempfile::tempdir().unwrap();
    let fake2 = spawn_stream_fake(tmp2.path(), HashMap::new(), vec![], true);
    std::fs::remove_file(&fake2.socket).unwrap();
    let (tmp2, addr2) = boot(enabled_terminal(), &fake2).await;
    let _ = tmp2;
    let ticket2 = mint_ticket(addr2, TERM_ID, None).await;
    let (status, text) = ws_http_get(
        addr2,
        TERM_ID,
        &ticket2,
        None,
        Some(&origin_header(addr2)),
        None,
    )
    .await;
    assert_eq!(status, 502, "runner unreachable must 502: {text}");
}

#[tokio::test]
async fn ws_opened_after_shutdown_closes_1012_immediately() {
    // Spec 8 #12 twin: a socket opened AFTER shutdown=true closes at
    // once with 1012 (the watch resolves immediately when already
    // true). The ticket is minted before shutdown posts.
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let fake = spawn_stream_fake(
        tmp.path(),
        replies,
        vec![s2c_envelope(ENV_TEXT, br#"{"t":"hello"}"#)],
        false,
    );
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let resp = client()
        .post(format!("{}/api/shutdown", base_url(addr)))
        .header("Origin", origin_header(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 202, "shutdown acknowledges");
    // The upgrade still completes (the daemon drains, not instant
    // teardown), and the relay closes it 1012 at once.
    let mut ws = match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(w) => w,
        other => panic!("connect after shutdown must upgrade, then 1012, got {other:?}"),
    };
    match ws_next(&mut ws, Duration::from_secs(5)).await {
        WsOutcome::Close(code, reason) => {
            assert_eq!(code, 1012, "after-shutdown close must be 1012");
            assert_eq!(reason, "service restart", "1012 reason");
        }
        other => panic!("after-shutdown socket must close 1012 immediately, got {other:?}"),
    }
}

#[tokio::test]
async fn term_state_live_on_sizechanged() {
    // A `sizechanged` frame emits exactly one live `term.state`;
    // the surrounding binary data frames emit none (low rate pin).
    let tmp = tempfile::tempdir().unwrap();
    let mut replies = HashMap::new();
    replies.insert("list".to_string(), list_reply(&[TERM_ID]));
    let script = vec![
        s2c_envelope(ENV_BINARY, &binary_frame(0, b"abc")),
        s2c_envelope(ENV_TEXT, br#"{"t":"sizechanged","cols":120,"rows":40}"#),
        s2c_envelope(ENV_BINARY, &binary_frame(3, b"def")),
    ];
    let fake = spawn_stream_fake(tmp.path(), replies, script, true);
    let (tmp, addr) = boot(enabled_terminal(), &fake).await;
    let _ = tmp;
    let ticket = mint_ticket(addr, TERM_ID, None).await;
    let mut ws = match ws_connect(addr, TERM_ID, &ticket, None, &origin_header(addr), None).await {
        WsConnect::Ok(w) => w,
        other => panic!("connect must succeed, got {other:?}"),
    };
    let (frames, close) = ws_read_n(&mut ws, 3).await;
    assert_eq!(
        frames.len(),
        3,
        "all three frames relay verbatim: {frames:?}"
    );
    assert_eq!(
        close,
        Some((1011, "upstream closed".to_string())),
        "fake closes with no exit: 1011"
    );
    drop(ws);
    let events = sse_term_events(addr, Duration::from_millis(1500)).await;
    let state: Vec<_> = events.iter().filter(|e| e.event == "term.state").collect();
    assert_eq!(
        state.len(),
        1,
        "exactly one term.state for the sizechanged: {events:?}"
    );
    assert_eq!(state[0].payload["id"], TERM_ID);
    assert_eq!(state[0].payload["state"], "live");
    assert_eq!(state[0].payload["cols"], 120);
    assert_eq!(state[0].payload["rows"], 40);
    assert!(
        events.iter().all(|e| e.event != "term.ended"),
        "no exit means no ended: {events:?}"
    );
    assert!(
        events.iter().all(|e| e.event != "term.created"),
        "no launch means no created: {events:?}"
    );
}
