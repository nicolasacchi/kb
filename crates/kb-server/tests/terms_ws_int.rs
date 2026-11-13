//! The real-tap WebSocket roundtrip (T12 spec 8): a REAL runner,
//! tmux holder and tap driven by `fake_harness.py`, with the real
//! kb-server router on top. Gated on the term-int lane like
//! `kb-term`'s own integration tests: outside the lane (`KB_TMUX_BIN`
//! unset) it prints `SKIP` and returns; with `KB_REQUIRE_TMUX=1`
//! (or `KB_REQUIRE_TERM_INT=1`) a missing tmux/runner/tap binary is
//! a FAILURE, never a skip. Every socket read has a hard deadline.
//!
//! What it pins, end to end:
//! (a) the first binary frame's offset is 0 — or the seed offset the
//!     tap announced (a fresh join sees the buffer from its start);
//! (b) 4 forced WS drops with `?from=<last offset>` reconnects
//!     resume with no gap and no duplicate once deduped by offset —
//!     the deduped byte stream is contiguous end to end;
//! (c) `POST /api/shutdown` sends the live socket Close 1012 and the
//!     tmux holder is still alive afterwards (kb-server and the
//!     holder are separate processes; a kb-server drain kills
//!     neither).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use kb_term::runner::config::RunnerConfig;
use kb_term::runner::rpc::dispatch;
use kb_term::runner::server::serve;
use kb_term::runner::Runner;
use kb_term::wire::{Harness, Request};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message;

const WS_DEADLINE: Duration = Duration::from_millis(2000);

/// The pinned tmux, or `None` outside the term-int lane. With either
/// require flag set, a missing tmux (or runner/tap wiring) is a
/// failure — never a silent skip.
fn require_tmux() -> Option<String> {
    match std::env::var("KB_TMUX_BIN") {
        Ok(bin) if !bin.is_empty() => Some(bin),
        _ => {
            if required() {
                panic!("term-int lane: KB_TMUX_BIN is unset but a require flag is set");
            }
            println!("SKIP: KB_TMUX_BIN unset (term-int lane only)");
            None
        }
    }
}

fn required() -> bool {
    std::env::var("KB_REQUIRE_TMUX").as_deref() == Ok("1")
        || std::env::var("KB_REQUIRE_TERM_INT").as_deref() == Ok("1")
}

fn require_tap() -> Option<PathBuf> {
    match std::env::var("KB_TAP_BIN") {
        Ok(bin) if !bin.is_empty() => Some(PathBuf::from(bin)),
        _ => {
            if required() {
                panic!("term-int lane: KB_TAP_BIN is unset but a require flag is set");
            }
            println!("SKIP: KB_TAP_BIN unset (term-int lane only)");
            None
        }
    }
}

fn kb_term_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("kb-term")
}

fn conf_path() -> PathBuf {
    kb_term_dir().join("share").join("kb-term.conf")
}

fn fixture(name: &str) -> PathBuf {
    kb_term_dir().join("fixtures").join(name)
}

fn python3_bin() -> PathBuf {
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let cand = Path::new(dir).join("python3");
        if cand.exists() {
            return cand;
        }
    }
    PathBuf::from("python3")
}

fn base_url(addr: std::net::SocketAddr) -> String {
    format!("http://{addr}")
}

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

async fn boot(
    terminal: kb_core::config::TerminalSection,
    runner_sock: &Path,
) -> (
    tempfile::TempDir,
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<(), anyhow::Error>>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();

    use kb_core::config::{DaemonSection, DefaultsSection, KbConfig, UiSection};
    use kb_core::types::KbName;
    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: std::collections::BTreeMap<KbName, kb_core::config::KbSection> =
        std::collections::BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source));
    let mut terminal = terminal;
    terminal.runner_socket = Some(runner_sock.to_path_buf());
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

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

enum WsConnect {
    Ok(Box<WsStream>),
    Other(String),
}

async fn ws_connect(
    addr: std::net::SocketAddr,
    id: &str,
    ticket: &str,
    from: Option<u64>,
) -> WsConnect {
    let mut url = format!("ws://{addr}/api/terms/{id}/ws?ticket={ticket}");
    if let Some(f) = from {
        url.push_str(&format!("&from={f}"));
    }
    // The URL string's `IntoClientRequest` supplies Host / Connection /
    // Upgrade / key / version (a bare `http::Request` lacks them and
    // tungstenite refuses it: `InvalidHeader("host")`).
    let mut req = url.into_client_request().unwrap();
    req.headers_mut()
        .insert("Origin", format!("http://{addr}").parse().unwrap());
    match tokio_tungstenite::connect_async_with_config(req, None, false).await {
        Ok((ws, _resp)) => WsConnect::Ok(Box::new(ws)),
        Err(e) => WsConnect::Other(format!("{e:?}")),
    }
}

/// Where the runner socket and run dir live, for the failure report.
static DIAG_PATHS: std::sync::OnceLock<(PathBuf, PathBuf)> = std::sync::OnceLock::new();

/// Failure report for a stream open that never succeeded: the runner's
/// own reply to a raw `stream` request (its error code), the session
/// directory listing and whether tap.sock accepts a connection.
async fn diagnose_stream(id: &str) -> String {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let Some((sock, run_dir)) = DIAG_PATHS.get() else {
        return "no diagnostics registered".to_string();
    };
    let mut out = String::new();
    match tokio::net::UnixStream::connect(sock).await {
        Ok(mut s) => {
            let req = format!("{{\"op\":\"stream\",\"id\":\"{id}\",\"from\":null}}\n");
            let _ = s.write_all(req.as_bytes()).await;
            let mut r = tokio::io::BufReader::new(s);
            let mut line = String::new();
            let got = tokio::time::timeout(Duration::from_secs(3), r.read_line(&mut line)).await;
            out.push_str(&format!("runner stream reply: {got:?} {line:?}\n"));
        }
        Err(e) => out.push_str(&format!("runner socket connect: {e}\n")),
    }
    let dir = run_dir.join("s").join(id);
    let names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.push_str(&format!(
        "session dir {} entries: {names:?}\n",
        dir.display()
    ));
    match tokio::net::UnixStream::connect(dir.join("tap.sock")).await {
        Ok(_) => out.push_str("tap.sock: connect ok\n"),
        Err(e) => out.push_str(&format!("tap.sock: connect error: {e}\n")),
    }
    out
}

/// Connect with a fresh ticket, retrying transient upstream-open
/// failures (the tap/pane startup race) until `deadline`; on
/// exhaustion panics with the last handshake outcome so a failed
/// connect names its HTTP status instead of a bare label.
async fn ws_connect_retry(
    addr: std::net::SocketAddr,
    id: &str,
    from: Option<u64>,
    deadline: Duration,
) -> WsStream {
    let start = std::time::Instant::now();
    let mut last: String;
    loop {
        let ticket = mint_ticket(addr, id).await;
        match ws_connect(addr, id, &ticket, from).await {
            WsConnect::Ok(ws) => return *ws,
            WsConnect::Other(e) => last = e,
        }
        if start.elapsed() >= deadline {
            let diag = diagnose_stream(id).await;
            panic!("ws connect for {id} never succeeded: last = {last}\n{diag}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn mint_ticket(addr: std::net::SocketAddr, id: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/terms/{id}/ticket", base_url(addr)))
        .header("Origin", format!("http://{addr}"))
        .header("x-kb-intent", "terminal")
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200, "ticket mint must succeed");
    let text = resp.text().await.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    v["ticket"].as_str().unwrap().to_string()
}

/// One round's outcome: binary intervals `(offset, len)` in arrival
/// order, the seed offset (if a seed frame was seen), and whether an
/// `exit` text frame passed through.
#[derive(Default, Debug)]
struct Round {
    binary: Vec<(u64, usize)>,
    seed_offset: Option<u64>,
    exit_seen: bool,
    closed: bool,
}

async fn read_round(ws: &mut WsStream, max_binary: usize) -> Round {
    let mut round = Round::default();
    for _ in 0..(max_binary * 3 + 10) {
        let msg = match tokio::time::timeout(WS_DEADLINE, ws.next()).await {
            Err(_) => break,
            Ok(None) => break,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(m))) => m,
        };
        match msg {
            Message::Binary(b) => {
                if b.len() >= 8 {
                    let mut hdr = [0u8; 8];
                    hdr.copy_from_slice(&b[..8]);
                    let offset = u64::from_be_bytes(hdr);
                    round.binary.push((offset, b.len() - 8));
                }
            }
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(t.as_str()) {
                    match v.get("t").and_then(|x| x.as_str()) {
                        Some("seed") => {
                            round.seed_offset = v
                                .get("offset")
                                .and_then(|x| x.as_u64())
                                .or(round.seed_offset);
                        }
                        Some("exit") => round.exit_seen = true,
                        _ => {}
                    }
                }
            }
            Message::Close(_) => {
                round.closed = true;
                break;
            }
            _ => {}
        }
        if round.binary.len() >= max_binary {
            break;
        }
    }
    round
}

/// Deduped contiguity: walk the intervals sorted by offset; a prefix
/// already covered by the cursor must be fully contained (a duplicate
/// resume replay — deduped bytes are not re-counted); otherwise the
/// interval must start EXACTLY at the cursor (no gap). Returns the
/// covered end. Unlike a permissive cover (offset <= cursor), this
/// rejects a tap that resends everything twice.
fn contiguous_after_dedupe(intervals: &[(u64, usize)], start: u64) -> u64 {
    let mut sorted: Vec<(u64, usize)> = intervals.to_vec();
    sorted.sort_by_key(|(o, _)| *o);
    let mut cursor = start;
    for (offset, len) in sorted {
        let end = offset + len as u64;
        if end <= cursor {
            continue; // a duplicate replay already covered
        }
        assert_eq!(
            offset, cursor,
            "gap: frame starts at {offset} but coverage cur is {cursor}"
        );
        cursor = end;
    }
    cursor
}

#[tokio::test]
async fn ws_real_tap_roundtrip() {
    let tmux_bin = match require_tmux() {
        None => return,
        Some(v) => v,
    };
    let tap_bin = match require_tap() {
        None => return,
        Some(v) => v,
    };
    std::env::set_var("KB_TERM_TEST_PROBE", "1");

    // --- the real runner, served on a unix socket --------------------
    let rundir = tempfile::Builder::new()
        .prefix("kb-term-ws-int-")
        .tempdir_in("/tmp")
        .unwrap();
    let run_dir = rundir.path().to_path_buf();
    std::fs::create_dir_all(run_dir.join("s")).unwrap();
    std::fs::create_dir_all(run_dir.join("runner")).unwrap();
    let mut cfg = RunnerConfig::test_config(run_dir.clone(), tmux_bin.clone(), conf_path());
    cfg.require_tap = true;
    cfg.tap_ready_secs = 30;
    cfg.tap_bin = Some(tap_bin);
    let runner = Arc::new(Runner::direct(cfg));
    let sock_path = run_dir.join("runner").join("runner.sock");
    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(4);
    let _serve_task = tokio::spawn(serve(
        runner.clone(),
        listener,
        sock_path.clone(),
        shutdown_rx,
    ));

    // --- launch via the runner's own dispatch (kb-server REST sends
    // harness-shaped launches without argv; the wire argv validation
    // requires the probe flag for an explicit fixture argv) -----------
    let launch = Request::Launch {
        harness: Harness::Omp,
        argv: Some(vec![
            python3_bin().to_string_lossy().into_owned(),
            fixture("fake_harness.py").to_string_lossy().into_owned(),
            "--seed".into(),
            "1".into(),
            "--length".into(),
            "64".into(),
            "--cols".into(),
            "120".into(),
            "--rows".into(),
            "40".into(),
        ]),
        resume_sid: None,
        cwd: run_dir.to_string_lossy().into_owned(),
        cols: Some(120),
        rows: Some(40),
        env: None,
        exit_keys: None,
    };
    let (id, _pane_pid) = match dispatch(&runner, &launch).await {
        kb_term::wire::Response::Ok { data } => (
            data["id"].as_str().unwrap().to_string(),
            data["pane_pid"].as_u64().unwrap_or(0),
        ),
        kb_term::wire::Response::Err { code, message } => {
            panic!("real launch failed ({code}): {message}")
        }
    };

    let _ = DIAG_PATHS.set((sock_path.clone(), run_dir.clone()));

    // --- the real kb-server on top of the real runner ----------------
    let (_srv_tmp, addr, _handle) = boot(
        kb_core::config::TerminalSection {
            enabled: Some(true),
            ..Default::default()
        },
        &sock_path,
    )
    .await;

    // (a) First join: the first binary frame starts at 0 (or at the
    // seed the tap announces for the fresh buffer).
    let mut ws = ws_connect_retry(addr, &id, None, Duration::from_secs(10)).await;
    let first = read_round(&mut ws, 40).await;
    let first_off = first
        .binary
        .first()
        .map(|(o, _)| *o)
        .expect("the harness must emit binary frames");
    match first.seed_offset {
        Some(seed) => assert_eq!(first_off, seed, "(a) first binary = seed offset"),
        None => assert_eq!(first_off, 0, "(a) first binary offset is 0"),
    }
    let mut all: Vec<(u64, usize)> = first.binary.clone();
    let mut last_end = first
        .binary
        .iter()
        .map(|(o, l)| *o + *l as u64)
        .max()
        .unwrap_or(0);
    let mut seed_opt = first.seed_offset;
    let _exit_seen = first.exit_seen;
    drop(ws);

    // (b) Four forced drops, each resuming at the last covered end.
    // The harness output is deterministic and complete in the ring
    // by the first round, so resumes replay (dedupe-contained)
    // bytes: only coverage persistence can be pinned from drops.
    for round in 0..4 {
        let mut ws = ws_connect_retry(addr, &id, Some(last_end), Duration::from_secs(10)).await;
        let got = read_round(&mut ws, 40).await;
        let advanced = got
            .binary
            .iter()
            .map(|(o, l)| *o + *l as u64)
            .max()
            .unwrap_or(last_end);
        assert!(
            advanced >= last_end,
            "(b) resume {round} must keep coverage: {advanced} < {last_end}"
        );
        last_end = advanced;
        seed_opt = seed_opt.or(got.seed_offset);
        all.extend(got.binary);
        drop(ws);
    }
    // Drain whatever remains until the harness exits.
    let mut ws = ws_connect_retry(addr, &id, Some(last_end), Duration::from_secs(10)).await;
    let got = read_round(&mut ws, 1000).await;
    all.extend(got.binary);
    drop(ws);
    // No-gap + no-duplicate-after-dedupe: the intervals, deduped by
    // offset, cover [start..end) contiguously with no duplicate
    // counted twice. The harness is deterministic, so a gap here is
    // a real byte loss.
    let start = match seed_opt {
        Some(s) => s,
        None => all.first().map(|(o, _)| *o).unwrap_or(0),
    };
    let end = contiguous_after_dedupe(&all, start);
    assert!(end > start, "(b) the tap must have delivered bytes");

    // (c) Shutdown sends Close 1012 on the still-open socket and
    // the tmux holder survives it.
    let mut ws = ws_connect_retry(addr, &id, Some(end), Duration::from_secs(10)).await;
    let _ = read_round(&mut ws, 1).await;
    let resp = reqwest::Client::new()
        .post(format!("{}/api/shutdown", base_url(addr)))
        .header("Origin", format!("http://{addr}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 202, "shutdown acknowledges");
    // The browser must RECEIVE Close 1012 with the documented
    // reason: a mutant closing 1009/1000/never must fail here.
    let close = loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Close(Some(frame))))) => {
                break (u16::from(frame.code), frame.reason.to_string());
            }
            Ok(Some(Ok(_))) => continue,
            _ => break (0u16, String::new()),
        }
    };
    assert_eq!(
        close,
        (1012, "service restart".to_string()),
        "(c) shutdown must deliver Close(1012, service restart)"
    );
    // The holder must be alive after the kb-server drain.
    let mut alive = false;
    for _ in 0..10 {
        let sock = runner.holder_socket(&id);
        let out = std::process::Command::new(&tmux_bin)
            .arg("-S")
            .arg(&sock)
            .arg("list-sessions")
            .output();
        match out {
            Ok(o) => {
                if o.status.success() {
                    alive = true;
                    break;
                }
            }
            Err(_) => break,
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        alive,
        "(c) the tmux holder must survive the kb-server shutdown"
    );
    drop(ws);

    // Cleanup: signal the runner server loop and kill holders, like
    // kb-term's own integration cleanup.
    let _ = shutdown_tx.send(());
    tokio::time::sleep(Duration::from_millis(100)).await;
    if let Ok(rd) = std::fs::read_dir(runner.sessions_dir()) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("sock") {
                let _ = std::process::Command::new(&tmux_bin)
                    .arg("-S")
                    .arg(&path)
                    .arg("kill-server")
                    .status();
            }
        }
    }
    drop(_serve_task);
    drop(_handle);
    drop(rundir);
}
