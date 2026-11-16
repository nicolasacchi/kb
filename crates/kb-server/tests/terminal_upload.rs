//! The phone Drive routes over the REAL router with a FAKE unix runner
//! (T18): `POST /api/terms/{id}/upload`, the closed `keys` vocabulary and
//! the optional drive `viewer`.
//!
//! The runner is a `tokio::net::UnixListener` task in the test's tempdir.
//! It counts connections, records every request line (and, for `upload`,
//! the raw body that follows it) and answers a scripted reply per op, so
//! "the gate never touched the runner" and "the runner got these exact
//! bytes" are observable assertions, not assumptions. Every picture is a
//! synthetic magic header plus filler generated here.

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, TerminalSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

const FIXTURE_TOKEN: &str = "test-token-fixture-deadbeef";
const TERM_ID: &str = "abcd1234";
/// The handler cap and the extractor slack, spelled out here on purpose:
/// a change to either constant must fail this file, not silently follow.
const CAP: usize = 8 * 1024 * 1024;
const SLACK: usize = 4096;

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

/// One recorded runner request: the parsed line and, for `upload`, the
/// body bytes that followed it.
#[derive(Clone)]
struct Seen {
    line: serde_json::Value,
    body: Vec<u8>,
}

struct FakeRunner {
    conns: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// The reply per op; swapped between requests by a test.
    replies: Arc<Mutex<std::collections::HashMap<String, String>>>,
    socket: std::path::PathBuf,
}

impl FakeRunner {
    fn set(&self, op: &str, reply: &str) {
        self.replies
            .lock()
            .unwrap()
            .insert(op.to_string(), reply.to_string());
    }

    fn ops(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.line["op"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

fn spawn_fake(dir: &std::path::Path) -> FakeRunner {
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let replies: Arc<Mutex<std::collections::HashMap<String, String>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let (c, s, r) = (conns.clone(), seen.clone(), replies.clone());
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            c.fetch_add(1, Ordering::SeqCst);
            let (s, r) = (s.clone(), r.clone());
            tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let parsed: serde_json::Value =
                    serde_json::from_str(line.trim()).unwrap_or_default();
                let op = parsed["op"].as_str().unwrap_or_default().to_string();
                let mut body = Vec::new();
                if op == "upload" {
                    let len = parsed["len"].as_u64().unwrap_or(0) as usize;
                    body = vec![0u8; len];
                    let read = tokio::time::timeout(
                        std::time::Duration::from_secs(20),
                        reader.read_exact(&mut body),
                    )
                    .await;
                    if !matches!(read, Ok(Ok(_))) {
                        return;
                    }
                }
                s.lock().unwrap().push(Seen { line: parsed, body });
                let reply = r
                    .lock()
                    .unwrap()
                    .get(&op)
                    .cloned()
                    .unwrap_or_else(|| "{\"status\":\"ok\",\"data\":{}}".to_string());
                let mut stream = reader.into_inner();
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.write_all(b"\n").await;
            });
        }
    });
    FakeRunner {
        conns,
        seen,
        replies,
        socket: sock,
    }
}

async fn boot(
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

fn enabled() -> TerminalSection {
    TerminalSection {
        enabled: Some(true),
        ..Default::default()
    }
}

fn base(addr: std::net::SocketAddr) -> String {
    format!("http://{addr}")
}

fn origin(addr: std::net::SocketAddr) -> String {
    format!("http://{addr}")
}

fn png(len: usize) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    v.resize(len.max(8), 0x5A);
    v
}

/// An upload request with the given `Content-Type` (none when `None`), valid
/// Origin and intent. `header()` APPENDS, so a caller that wants a different
/// content type must start from here rather than add a second one.
fn upload_req_ct(
    addr: std::net::SocketAddr,
    id: &str,
    body: Vec<u8>,
    content_type: Option<&str>,
) -> reqwest::RequestBuilder {
    let mut req = reqwest::Client::new()
        .post(format!("{}/api/terms/{id}/upload", base(addr)))
        .header("Origin", origin(addr))
        .header("x-kb-intent", "terminal")
        .body(body);
    if let Some(ct) = content_type {
        req = req.header("Content-Type", ct);
    }
    req
}

/// A fully valid upload request (Origin, intent, `image/png`).
fn upload_req(addr: std::net::SocketAddr, id: &str, body: Vec<u8>) -> reqwest::RequestBuilder {
    upload_req_ct(addr, id, body, Some("image/png"))
}

async fn status_and_text(resp: reqwest::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    (status, resp.text().await.unwrap_or_default())
}

fn assert_problem(text: &str, urn_tail: &str) {
    assert!(
        text.contains(urn_tail),
        "response must carry {urn_tail}, got: {text}"
    );
}

const UPLOAD_OK: &str = "{\"status\":\"ok\",\"data\":{\"path\":\"/run/kb-term/s/abcd1234/up/6a57254b1191f4da3a69d9d89ad2d844463324e783cda4058ea9c292e844b82c.png\",\"bytes\":64,\"mime\":\"image/png\",\"sha256\":\"6a57254b1191f4da3a69d9d89ad2d844463324e783cda4058ea9c292e844b82c\"}}";

// ------------------------------------------------------------------ gate

#[tokio::test]
async fn a_disabled_daemon_answers_a_bodiless_404_and_never_dials_the_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let (_tmp, addr) = boot(TerminalSection::default(), &fake, false).await;
    let resp = upload_req(addr, TERM_ID, png(64)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    assert!(
        resp.headers().get("content-type").is_none(),
        "bodiless: no content-type"
    );
    let (_s, text) = status_and_text(resp).await;
    assert!(text.is_empty(), "bodiless, got: {text}");
    // The same holds for a body over the extractor limit: a disabled
    // daemon must not answer 413 and thereby name a size limit.
    let resp = upload_req(addr, TERM_ID, vec![0u8; CAP + SLACK + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404, "disabled beats the size limit");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_shared_gate_runs_before_anything_else_on_the_upload_route() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let (_tmp, addr) = boot(enabled(), &fake, false).await;
    let c = reqwest::Client::new();
    let url = format!("{}/api/terms/{TERM_ID}/upload", base(addr));

    // Absent Origin (loopback does NOT bypass: every request here is from
    // loopback) -> terminal-origin-refused.
    let resp = c
        .post(&url)
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "image/png")
        .body(png(64))
        .send()
        .await
        .unwrap();
    let (s, text) = status_and_text(resp).await;
    assert_eq!(s, 403);
    assert_problem(&text, "terminal-origin-refused");

    // A wrong Origin, an artifact-subdomain Origin.
    for bad in ["https://evil.example", "http://x.artifacts.localhost"] {
        let resp = c
            .post(&url)
            .header("Origin", bad)
            .header("x-kb-intent", "terminal")
            .header("Content-Type", "image/png")
            .body(png(64))
            .send()
            .await
            .unwrap();
        let (s, text) = status_and_text(resp).await;
        assert_eq!(s, 403, "origin {bad}");
        assert_problem(&text, "terminal-origin-refused");
    }

    // No intent header, and a wrong one.
    for intent in [None, Some("nope")] {
        let mut req = c
            .post(&url)
            .header("Origin", origin(addr))
            .header("Content-Type", "image/png")
            .body(png(64));
        if let Some(v) = intent {
            req = req.header("x-kb-intent", v);
        }
        let (s, text) = status_and_text(req.send().await.unwrap()).await;
        assert_eq!(s, 403, "intent {intent:?}");
        assert_problem(&text, "terminal-intent-required");
    }

    // The gate also beats the 413 and the 415: a refused caller learns
    // nothing about the limit or the accepted types.
    let resp = c
        .post(&url)
        .body(vec![0u8; CAP + SLACK + 1])
        .send()
        .await
        .unwrap();
    let (s, text) = status_and_text(resp).await;
    assert_eq!(s, 403, "an oversize body from a refused caller is a 403");
    assert_problem(&text, "terminal-origin-refused");
    let resp = c
        .post(&url)
        .header("Content-Type", "application/json")
        .body("{}")
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

#[tokio::test]
async fn a_bearer_token_caller_is_refused_on_the_upload_route() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let (_tmp, addr) = boot(enabled(), &fake, true).await;
    let resp = upload_req(addr, TERM_ID, png(64))
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    let (s, text) = status_and_text(resp).await;
    assert_eq!(s, 403);
    assert_problem(&text, "terminal-token-refused");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

// ---------------------------------------------------------------- limits

#[tokio::test]
async fn only_the_four_picture_content_types_and_valid_ids_reach_the_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let (_tmp, addr) = boot(enabled(), &fake, false).await;
    for ct in [
        "application/json",
        "image/svg+xml",
        "text/html",
        "application/pdf",
        "multipart/form-data; boundary=x",
        "application/octet-stream",
    ] {
        let resp = upload_req_ct(addr, TERM_ID, png(64), Some(ct))
            .send()
            .await
            .unwrap();
        let (s, text) = status_and_text(resp).await;
        assert_eq!(s, 415, "content-type {ct}");
        assert_problem(&text, "unsupported-media");
    }
    // No Content-Type at all.
    let resp = upload_req_ct(addr, TERM_ID, png(64), None)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 415);
    // The kb-side id grammar (one validator, R6): out of grammar -> 400.
    for id in ["A1", "a%20b", "..%2Fx", "a".repeat(33).as_str()] {
        let resp = upload_req(addr, id, png(64)).send().await.unwrap();
        let (s, text) = status_and_text(resp).await;
        assert_eq!(s, 400, "id {id}");
        assert_problem(&text, "bad-request");
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_handler_refuses_a_body_one_byte_over_the_cap_and_admits_exactly_the_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.set("upload", UPLOAD_OK);
    let (_tmp, addr) = boot(enabled(), &fake, false).await;
    // Over the cap but inside the extractor's slack: the handler's own
    // check answers 413, without dialing the runner.
    let resp = upload_req(addr, TERM_ID, png(CAP + 1))
        .send()
        .await
        .unwrap();
    let (s, text) = status_and_text(resp).await;
    assert_eq!(s, 413);
    assert_problem(&text, "payload-too-large");
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "refused before the runner"
    );
    // Exactly the cap goes through.
    let resp = upload_req(addr, TERM_ID, png(CAP)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].body.len(), CAP, "the runner got all 8 MiB");
}

#[tokio::test]
async fn a_body_past_the_extractor_limit_is_413_and_never_reaches_the_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let (_tmp, addr) = boot(enabled(), &fake, false).await;
    let resp = upload_req(addr, TERM_ID, png(CAP + SLACK + 1))
        .send()
        .await
        .unwrap();
    let (s, text) = status_and_text(resp).await;
    assert_eq!(s, 413);
    assert_problem(&text, "payload-too-large");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

// ----------------------------------------------------------------- relay

#[tokio::test]
async fn the_runner_gets_the_exact_bytes_and_the_response_is_the_typed_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    fake.set("upload", UPLOAD_OK);
    let (_tmp, addr) = boot(enabled(), &fake, false).await;
    let bytes = png(5000);
    let resp = upload_req_ct(
        addr,
        TERM_ID,
        bytes.clone(),
        Some("Image/PNG; charset=binary"),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    let (_s, text) = status_and_text(resp).await;
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let keys: std::collections::BTreeSet<&str> =
        v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["bytes", "mime", "path", "sha256"].into_iter().collect(),
        "exactly the typed shape"
    );
    assert_eq!(v["mime"], "image/png");
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "exactly one runner connection");
    assert_eq!(
        seen[0].line,
        serde_json::json!({"op":"upload","id":TERM_ID,"mime":"image/png","len":5000}),
        "the declared type is the normalised media type, the id is the path id"
    );
    assert_eq!(seen[0].body, bytes, "the body arrives verbatim");
}

#[tokio::test]
async fn runner_refusals_map_to_statuses_and_a_dead_runner_is_a_503() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let (_tmp, addr) = boot(enabled(), &fake, false).await;
    for (code, want) in [
        ("bad-image", 415u16),
        ("too-large", 413),
        ("not-found", 404),
        ("invalid-id", 400),
        ("io", 502),
        ("internal", 502),
    ] {
        fake.set(
            "upload",
            &format!("{{\"status\":\"err\",\"code\":\"{code}\",\"message\":\"synthetic {code}\"}}"),
        );
        let resp = upload_req(addr, TERM_ID, png(64)).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), want, "runner code {code}");
    }
    // A reply that is not the typed shape is a bad gateway, not a 200.
    fake.set("upload", "{\"status\":\"ok\",\"data\":{\"path\":1}}");
    let resp = upload_req(addr, TERM_ID, png(64)).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 502);

    // No socket at all -> 503, and the socket path never leaks.
    let tmp2 = tempfile::tempdir().unwrap();
    let missing = tmp2.path().join("absent.sock");
    let dead = FakeRunner {
        conns: Arc::new(AtomicUsize::new(0)),
        seen: Arc::new(Mutex::new(Vec::new())),
        replies: Arc::new(Mutex::new(Default::default())),
        socket: missing.clone(),
    };
    let (_tmp3, addr2) = boot(enabled(), &dead, false).await;
    let resp = upload_req(addr2, TERM_ID, png(64)).send().await.unwrap();
    let (s, text) = status_and_text(resp).await;
    assert_eq!(s, 503);
    assert!(!text.contains(&missing.to_string_lossy().into_owned()));
    assert_problem(&text, "terminal-runner-unavailable");
}

// ------------------------------------------------------------ keys + viewer

async fn post_json(addr: std::net::SocketAddr, suffix: &str, body: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/api/terms/{TERM_ID}{suffix}", base(addr)))
        .header("Origin", origin(addr))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn the_keys_vocabulary_is_closed_and_every_name_is_forwarded_as_named() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let (_tmp, addr) = boot(enabled(), &fake, false).await;
    // Unknown names: 400 bad-key, nothing forwarded. Lookalikes of the
    // runner's names, a free-form string, an empty one.
    for bad in [
        "F1", "Escape", "BTab", "/", "C-x", "esc", "", "a", "0", "Enter ",
    ] {
        let resp = post_json(addr, "/keys", &format!("{{\"keys\":[\"{bad}\"]}}")).await;
        let (s, text) = status_and_text(resp).await;
        assert_eq!(s, 400, "key {bad:?}");
        assert_problem(&text, "bad-key");
    }
    // One bad name among good ones still refuses the whole request.
    let resp = post_json(addr, "/keys", "{\"keys\":[\"Enter\",\"F1\"]}").await;
    assert_eq!(resp.status().as_u16(), 400);
    // More than 32 keys, and none.
    let many = vec!["\"Tab\""; 33].join(",");
    let resp = post_json(addr, "/keys", &format!("{{\"keys\":[{many}]}}")).await;
    assert_eq!(resp.status().as_u16(), 400, "33 keys");
    let resp = post_json(addr, "/keys", "{\"keys\":[]}").await;
    assert_eq!(resp.status().as_u16(), 400, "no keys");
    assert_eq!(
        fake.ops(),
        Vec::<String>::new(),
        "nothing reached the runner"
    );
    // Every allowed name is forwarded, in order, as a named key.
    let names = [
        "Esc", "Tab", "S-Tab", "Enter", "Up", "Down", "Right", "Left", "C-c", "C-d", "C-u",
        "BSpace", "Slash", "1", "2", "3", "4", "5", "6", "7", "8", "9", "y", "n",
    ];
    for name in names {
        let resp = post_json(addr, "/keys", &format!("{{\"keys\":[\"{name}\"]}}")).await;
        assert_eq!(resp.status().as_u16(), 200, "key {name}");
    }
    let forwarded: Vec<String> = fake
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.line["op"] == "keys")
        .map(|s| s.line["input"]["named"][0].as_str().unwrap().to_string())
        .collect();
    assert_eq!(forwarded, names);
}

#[tokio::test]
async fn the_drive_viewer_is_validated_and_reported_to_the_tap_only_after_success() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path());
    let (_tmp, addr) = boot(enabled(), &fake, false).await;
    let viewer = "ws-abcd123456";

    // A bad viewer id is a 400 before any runner call, on all four routes.
    for (suffix, body) in [
        ("/paste", "{\"text\":\"hi\",\"viewer\":\"Bad Id\"}"),
        ("/enter", "{\"viewer\":\"../x\"}"),
        ("/keys", "{\"keys\":[\"Tab\"],\"viewer\":\"\"}"),
        ("/take-size", "{\"cols\":80,\"rows\":24,\"viewer\":\"A\"}"),
    ] {
        let resp = post_json(addr, suffix, body).await;
        let (s, text) = status_and_text(resp).await;
        assert_eq!(s, 400, "{suffix}");
        assert_problem(&text, "bad-request");
    }
    assert_eq!(fake.ops(), Vec::<String>::new());

    // A good viewer: the input op runs, THEN a typed notification names it.
    for (suffix, body, op) in [
        (
            "/paste",
            format!("{{\"text\":\"hi\",\"viewer\":\"{viewer}\"}}"),
            "paste",
        ),
        ("/enter", format!("{{\"viewer\":\"{viewer}\"}}"), "enter"),
        (
            "/keys",
            format!("{{\"keys\":[\"Tab\"],\"viewer\":\"{viewer}\"}}"),
            "keys",
        ),
        (
            "/take-size",
            format!("{{\"cols\":80,\"rows\":24,\"viewer\":\"{viewer}\"}}"),
            "take_size",
        ),
    ] {
        fake.seen.lock().unwrap().clear();
        let resp = post_json(addr, suffix, &body).await;
        assert_eq!(resp.status().as_u16(), 200, "{suffix}");
        let ops = fake.ops();
        assert_eq!(
            ops.last().map(String::as_str),
            Some("typed"),
            "{suffix}: {ops:?}"
        );
        assert!(ops.contains(&op.to_string()), "{suffix}: {ops:?}");
        let typed = fake
            .seen
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.line["op"] == "typed")
            .unwrap()
            .line
            .clone();
        assert_eq!(typed["id"], TERM_ID);
        assert_eq!(typed["viewer"], viewer);
    }

    // Without a viewer there is no typed op (the pre-T18 shape).
    fake.seen.lock().unwrap().clear();
    let resp = post_json(addr, "/keys", "{\"keys\":[\"Tab\"]}").await;
    assert_eq!(resp.status().as_u16(), 200);
    assert!(!fake.ops().contains(&"typed".to_string()));

    // A refused input is not typing: the runner says paste-only/blocked and
    // the tap hears nothing.
    fake.seen.lock().unwrap().clear();
    fake.set(
        "paste",
        "{\"status\":\"err\",\"code\":\"multi-line-refused\",\"message\":\"synthetic\"}",
    );
    let resp = post_json(
        addr,
        "/paste",
        &format!("{{\"text\":\"a\\nb\",\"viewer\":\"{viewer}\"}}"),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 422);
    assert!(
        !fake.ops().contains(&"typed".to_string()),
        "a refused paste must not mark the viewer as typing: {:?}",
        fake.ops()
    );

    // A failing typed notification never fails the input that succeeded.
    fake.set(
        "typed",
        "{\"status\":\"err\",\"code\":\"tap-timeout\",\"message\":\"synthetic\"}",
    );
    fake.set("keys", "{\"status\":\"ok\",\"data\":{}}");
    let resp = post_json(
        addr,
        "/keys",
        &format!("{{\"keys\":[\"Tab\"],\"viewer\":\"{viewer}\"}}"),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 200);
}
