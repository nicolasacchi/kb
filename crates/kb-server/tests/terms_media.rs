//! `GET /api/terms/media/{sha256}` over the REAL router with a FAKE unix
//! runner that speaks the media reply shape: one envelope header line
//! `{"status":"ok","data":{"len":N}}` followed by exactly N raw bytes
//! (or one `err` line with no bytes).
//!
//! The fake counts connections, so "the request never reached the
//! runner" (disabled tier, malformed hash, refused gate) is an
//! observable assertion rather than an assumption.

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, TerminalSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

const FIXTURE_TOKEN: &str = "test-token-fixture-deadbeef";

/// A 1x1 PNG (magic + IHDR/IDAT/IEND skeleton is enough: the route
/// sniffs the signature, it never decodes).
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
];
const SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"/>";

fn sha(c: char) -> String {
    c.to_string().repeat(64)
}

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

/// One scripted stored picture: the bytes plus a `mime` the fake puts in
/// the header `data` (a stored mime the route must IGNORE).
#[derive(Clone)]
struct Stored {
    bytes: Vec<u8>,
    mime: &'static str,
}

struct FakeRunner {
    conns: Arc<AtomicUsize>,
    socket: std::path::PathBuf,
}

/// Serve `media` requests from `store` (unknown hash -> `not-found`).
fn spawn_fake(dir: &std::path::Path, store: HashMap<String, Stored>) -> FakeRunner {
    let sock = dir.join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let counter = conns.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => return,
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let store = store.clone();
            tokio::spawn(async move {
                let mut line = String::new();
                let mut reader = tokio::io::BufReader::new(stream);
                if reader.read_line(&mut line).await.is_err() {
                    return;
                }
                let req: serde_json::Value = serde_json::from_str(line.trim()).unwrap_or_default();
                let key = req
                    .get("sha256")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let is_media = req.get("op").and_then(|v| v.as_str()) == Some("media");
                let mut stream = reader.into_inner();
                match store.get(&key) {
                    Some(s) if is_media => {
                        let head = format!(
                            "{{\"v\":1,\"id\":null,\"status\":\"ok\",\"data\":{{\"len\":{},\"mime\":\"{}\"}}}}\n",
                            s.bytes.len(),
                            s.mime
                        );
                        let _ = stream.write_all(head.as_bytes()).await;
                        let _ = stream.write_all(&s.bytes).await;
                    }
                    _ => {
                        let _ = stream
                            .write_all(
                                b"{\"status\":\"err\",\"code\":\"not-found\",\"message\":\"no such picture\"}\n",
                            )
                            .await;
                    }
                }
            });
        }
    });
    FakeRunner {
        conns,
        socket: sock,
    }
}

/// A runner that accepts and never answers (for the timeout case).
fn spawn_silent(dir: &std::path::Path) -> FakeRunner {
    let sock = dir.join("silent.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let conns = Arc::new(AtomicUsize::new(0));
    let counter = conns.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    FakeRunner {
        conns,
        socket: sock,
    }
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

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn url(addr: std::net::SocketAddr, key: &str) -> String {
    format!("http://{addr}/api/terms/media/{key}")
}

fn origin_of(addr: std::net::SocketAddr) -> String {
    format!("http://{addr}")
}

fn one_png() -> HashMap<String, Stored> {
    let mut m = HashMap::new();
    m.insert(
        sha('a'),
        Stored {
            bytes: PNG.to_vec(),
            mime: "image/png",
        },
    );
    m
}

/// A GET carrying the intent header and (optionally) the exact Origin.
fn get_ok(c: &reqwest::Client, addr: std::net::SocketAddr, key: &str) -> reqwest::RequestBuilder {
    c.get(url(addr, key)).header("x-kb-intent", "terminal")
}

fn problem_type(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str()).map(String::from))
        .unwrap_or_default()
}

#[tokio::test]
async fn disabled_returns_bodiless_404() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(TerminalSection::default(), &fake, false).await;
    let c = client();
    // Valid hash, intent and Origin: only the disabled tier answers.
    let resp = get_ok(&c, addr, &sha('a'))
        .header("Origin", origin_of(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    assert!(resp.headers().get("content-type").is_none());
    let body = resp.bytes().await.unwrap();
    assert!(body.is_empty(), "disabled must be bodiless");
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rejects_non_hex64_and_uppercase_and_63_65_chars_with_400() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let bad = [
        "A".repeat(64),
        "a".repeat(63),
        "a".repeat(65),
        format!("{}g", "a".repeat(63)),
        "e3b0".to_string(),
    ];
    for key in bad {
        let resp = get_ok(&c, addr, &key).send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 400, "{key}");
    }
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "a malformed hash never reaches the runner"
    );
}

#[tokio::test]
async fn traversal_strings_never_reach_the_runner() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for key in [
        "%2e%2e%2f%2e%2e%2fetc%2fpasswd",
        "..%2f..%2f..%2fetc%2fpasswd",
        "%2fetc%2fpasswd",
        "aaaa%00",
        "..%5c..%5cwindows",
    ] {
        let resp = get_ok(&c, addr, key).send().await.unwrap();
        let status = resp.status().as_u16();
        assert!(
            (400..500).contains(&status),
            "{key} must be a 4xx, got {status}"
        );
    }
    assert_eq!(
        fake.conns.load(Ordering::SeqCst),
        0,
        "traversal strings must never reach the runner"
    );
}

#[tokio::test]
async fn missing_intent_header_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    // Right Origin, no intent: refused. (An <img src> or a cross-site
    // navigation carries neither header.)
    for with_origin in [true, false] {
        let mut req = c.get(url(addr, &sha('a')));
        if with_origin {
            req = req.header("Origin", origin_of(addr));
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status().as_u16(), 403, "origin={with_origin}");
        let text = resp.text().await.unwrap();
        assert!(
            problem_type(&text).ends_with("terminal-intent-required"),
            "{text}"
        );
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn wrong_origin_is_refused_absent_origin_with_intent_is_admitted() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for origin in [
        "http://evil.example",
        "http://abcd1234.artifacts.localhost:4000",
        "null",
    ] {
        let resp = get_ok(&c, addr, &sha('a'))
            .header("Origin", origin)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{origin}");
        let text = resp.text().await.unwrap();
        assert!(
            problem_type(&text).ends_with("terminal-origin-refused"),
            "{origin}: {text}"
        );
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
    // Absent Origin + intent: the same-origin fetch GET shape.
    let resp = get_ok(&c, addr, &sha('a')).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    // The exact same-host Origin is admitted too.
    let resp = get_ok(&c, addr, &sha('a'))
        .header("Origin", origin_of(addr))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[tokio::test]
async fn cross_site_sec_fetch_site_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    for site in ["cross-site", "same-site"] {
        let resp = get_ok(&c, addr, &sha('a'))
            .header("Sec-Fetch-Site", site)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 403, "{site}");
    }
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
    for site in ["same-origin", "none"] {
        let resp = get_ok(&c, addr, &sha('a'))
            .header("Sec-Fetch-Site", site)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200, "{site}");
    }
}

#[tokio::test]
async fn token_callers_are_refused_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(enabled_terminal(), &fake, true).await;
    let c = client();
    let resp = get_ok(&c, addr, &sha('a'))
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let text = resp.text().await.unwrap();
    assert!(
        problem_type(&text).ends_with("terminal-token-refused"),
        "{text}"
    );
    assert_eq!(fake.conns.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn png_served_with_nosniff_immutable_csp() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = get_ok(&c, addr, &sha('a')).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let h = resp.headers().clone();
    let get = |n: &str| {
        h.get(n)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(get("content-type"), "image/png");
    assert_eq!(get("x-content-type-options"), "nosniff");
    assert_eq!(get("cache-control"), "private, max-age=31536000, immutable");
    assert_eq!(
        get("content-security-policy"),
        "default-src 'none'; sandbox"
    );
    assert_eq!(get("content-disposition"), "inline");
    assert_eq!(resp.bytes().await.unwrap().as_ref(), PNG);
}

#[tokio::test]
async fn svg_bytes_are_415_even_if_stored_mime_says_image_png() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = HashMap::new();
    store.insert(
        sha('b'),
        Stored {
            bytes: SVG.to_vec(),
            mime: "image/png",
        },
    );
    let fake = spawn_fake(tmp.path(), store);
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = get_ok(&c, addr, &sha('b')).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 415);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(!ct.starts_with("image/"), "an SVG is never served: {ct}");
    let text = resp.text().await.unwrap();
    assert!(!text.contains("<svg"), "the SVG body must not be echoed");
}

#[tokio::test]
async fn bytes_sniffed_as_png_win_over_a_wrong_stored_mime() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = HashMap::new();
    store.insert(
        sha('c'),
        Stored {
            bytes: PNG.to_vec(),
            mime: "image/svg+xml",
        },
    );
    let fake = spawn_fake(tmp.path(), store);
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = get_ok(&c, addr, &sha('c')).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("image/png")
    );
}

#[tokio::test]
async fn unknown_hash_is_404() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_fake(tmp.path(), one_png());
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let resp = get_ok(&c, addr, &sha('f')).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(fake.conns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn runner_down_is_502_not_a_hang() {
    let tmp = tempfile::tempdir().unwrap();
    // A socket path nobody listens on.
    let fake = FakeRunner {
        conns: Arc::new(AtomicUsize::new(0)),
        socket: tmp.path().join("absent.sock"),
    };
    let (_t, addr) = boot(enabled_terminal(), &fake, false).await;
    let c = client();
    let started = std::time::Instant::now();
    let resp = get_ok(&c, addr, &sha('a')).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let text = resp.text().await.unwrap();
    assert!(
        !text.contains("absent.sock"),
        "the socket path must never leak: {text}"
    );
}

#[tokio::test]
async fn silent_runner_is_502_within_the_client_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = spawn_silent(tmp.path());
    let terminal = TerminalSection {
        enabled: Some(true),
        runner_timeout_secs: Some(1),
        ..Default::default()
    };
    let (_t, addr) = boot(terminal, &fake, false).await;
    let c = client();
    let started = std::time::Instant::now();
    let resp = get_ok(&c, addr, &sha('a')).send().await.unwrap();
    assert_eq!(resp.status().as_u16(), 502);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "the relay must give up at the configured runner timeout"
    );
}
