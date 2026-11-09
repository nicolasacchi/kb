//! term-int integration tests for the T16 mirror (`kb-term attach`).
//!
//! These drive the real `kb-term` binary on a pty against a fake tap
//! (speaking the T1/T7 wire: `kb-term/1` frames + tap.sock envelopes)
//! and a fake runner (runner.sock JSON RPC). The fake tap's stream is
//! synthetic: S seed lines then L live lines with 3 framed rebuilds,
//! every 50th line a placeholder-row picture (shapes from
//! `fixtures/synth_omp_kitty_stream.py`). The checks port
//! `tools/mirror_standin.py`'s `verify_stream`/`analyze_dump`.
//!
//! Every pty/socket read has a hard deadline; the whole test stays
//! well under any lane timeout. With `KB_TMUX_BIN` unset the tests
//! early-return (pass) so the `workspace-test` lane stays green;
//! with `KB_REQUIRE_TMUX=1` an absent tmux FAILS (same shape as the
//! other term-int lanes — the tests gate on the term-int env, never
//! on `CI`).

use std::io::Read;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kb_term::wire::{EnvelopeDecoder, ServerFrame, ENV_BINARY, ENV_CONTROL, ENV_TEXT};

// ------------------------------------------------------------------ gate --

fn term_int_gate() -> bool {
    let require = std::env::var("KB_REQUIRE_TMUX").is_ok_and(|v| v == "1");
    let has = std::env::var("KB_TMUX_BIN")
        .is_ok_and(|v| !v.is_empty() && std::path::Path::new(&v).exists());
    if !has {
        if require {
            panic!("KB_REQUIRE_TMUX=1 but KB_TMUX_BIN is unset or missing");
        }
        return false;
    }
    true
}

// ------------------------------------------------------------------- dirs --

static CTR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> TempDir {
        let n = CTR.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("kbt-t16-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// --------------------------------------------------------------- fixtures --

const PLACEHOLDER: [u8; 4] = [0xF4, 0x8E, 0xBB, 0xAE];
const DIACRITICS: &[u32] = &[
    773, 781, 782, 784, 786, 829, 830, 831, 838, 842, 843, 844, 848, 849, 850, 855, 859, 867, 868,
    869, 870, 871, 872, 873, 874, 875, 876, 877, 878, 879, 1155, 1156, 1157, 1158, 1159, 1426,
    1427, 1428, 1429, 1431, 1432, 1433, 1436, 1437, 1438, 1439, 1440, 1441, 1448, 1449, 1451, 1452,
    1455, 1476, 1552, 1553, 1554, 1555, 1556, 1557, 1558, 1559, 1623, 1624, 1625, 1626, 1627, 1629,
    1630, 1750, 1751, 1752, 1753, 1754, 1755, 1756, 1759, 1760, 1761, 1762, 1764, 1767, 1768, 1771,
    1772, 1840, 1842, 1843, 1845, 1846, 1850, 1853, 1855, 1856, 1857, 1859, 1861, 1863, 1865, 1866,
    2027, 2028, 2029, 2030, 2031, 2032, 2033, 2035, 2070, 2071, 2072, 2073, 2075, 2076, 2077, 2078,
    2079, 2080, 2081, 2082, 2083, 2085, 2086, 2087, 2089, 2090, 2091, 2092, 2093, 2385, 2387, 2388,
    3970, 3971, 3974, 3975, 4957, 4958, 4959, 6109, 6458, 6679, 6773, 6774, 6775, 6776, 6777, 6778,
    6779, 6780, 7019, 7021, 7022, 7023, 7024, 7025,
];

fn char_bytes(cp: u32) -> Vec<u8> {
    let mut buf = [0u8; 4];
    char::from_u32(cp)
        .unwrap_or('\u{301}')
        .encode_utf8(&mut buf)
        .as_bytes()
        .to_vec()
}

fn dummy_rgb(n: u32) -> (u8, u8, u8) {
    let pal: [(u8, u8, u8); 6] = [
        (0x66, 0xCF, 0x38),
        (0x21, 0xA0, 0xF0),
        (0xE0, 0x33, 0x10),
        (0x10, 0xC8, 0x6E),
        (0xEE, 0x22, 0x88),
        (0x11, 0x11, 0xDD),
    ];
    if n > 0 && n % 5 == 4 {
        // Adjacent same-colour picture (the rule the diacritic-0 split
        // must still see as a new picture).
        pal[((n - 1) % 6) as usize]
    } else {
        pal[(n % 6) as usize]
    }
}

fn dummy_id(n: u32) -> u32 {
    let (r, g, b) = dummy_rgb(n);
    ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

fn picture_line(n: u32, cols: usize) -> Vec<u8> {
    let (r, g, b) = dummy_rgb(n);
    let mut v = Vec::new();
    v.extend_from_slice(format!("L{n:06} ").as_bytes());
    v.extend_from_slice(format!("\x1b[38;2;{r};{g};{b}m").as_bytes());
    v.extend_from_slice(format!("\x1b[58:2::{r}:{g}:{b}m").as_bytes());
    for c in 0..cols {
        v.extend_from_slice(&PLACEHOLDER);
        v.extend_from_slice(&char_bytes(DIACRITICS[0]));
        v.extend_from_slice(&char_bytes(DIACRITICS[c % DIACRITICS.len()]));
    }
    v.extend_from_slice(b"\x1b[39;59m");
    v
}

fn stream_line(n: u32, cols: usize) -> Vec<u8> {
    if n.is_multiple_of(50) {
        picture_line(n, cols)
    } else {
        format!("L{n:06} line {n} lorem ipsum dolor sit amet").into_bytes()
    }
}

/// (seed, live) bytes for the T4 stream: S numbered seed lines (no ED3
/// inside the seed), then L live lines with 3 framed rebuilds at live
/// offsets 100/200/300.
fn build_t4(cols: usize, seed: u32, live: u32) -> (Vec<u8>, Vec<u8>) {
    let mut seed_bytes = Vec::new();
    for n in 1..=seed {
        seed_bytes.extend_from_slice(&stream_line(n, cols));
        seed_bytes.extend_from_slice(b"\r\n");
    }
    let mut live_bytes = Vec::new();
    let mut count = seed;
    for i in 1..=live {
        count += 1;
        live_bytes.extend_from_slice(&stream_line(count, cols));
        live_bytes.extend_from_slice(b"\r\n");
        if [100, 200, 300].contains(&i) {
            live_bytes.extend_from_slice(b"\x1b[?2026h\x1b[H\x1b[2J\x1b[3J");
            for k in 1..=count {
                live_bytes.extend_from_slice(&stream_line(k, cols));
                live_bytes.extend_from_slice(b"\r\n");
            }
            live_bytes.extend_from_slice(b"\x1b[?2026l");
        }
    }
    (seed_bytes, live_bytes)
}

fn hello_frame(session: &str) -> ServerFrame {
    ServerFrame::Hello {
        proto: kb_term::wire::PROTO.into(),
        session: session.to_string(),
        epoch: "00000001".into(),
        harness: kb_term::wire::Harness::Omp,
        bound_sid: None,
    }
}

fn profile_frame(cols: u16, rows: u16) -> ServerFrame {
    ServerFrame::Profile {
        xterm: "6.0.0".into(),
        serialize: "0.14.0".into(),
        width: "unicode-graphemes@0.4.0".into(),
        image: "0.9.0".into(),
        cols,
        rows,
        scrollback: 50000,
        scroll_on_erase_in_display: true,
        cell_px_w: 9,
        cell_px_h: 18,
        theme: None,
    }
}

fn image_defs() -> Vec<ServerFrame> {
    let mut out = Vec::new();
    for n in 0u32..6 {
        out.push(ServerFrame::Imagedef {
            seq: 0,
            offset: 0,
            id: dummy_id(n),
            gen: 1,
            sha256: format!("{n:064x}"),
            mime: "image/png".to_string(),
            w: 640 + n * 10,
            h: 400 + n * 5,
            cols: 120,
            rows: 40,
        });
    }
    out
}

fn text_env(viewer: &str, frame: &ServerFrame) -> Vec<u8> {
    let json = serde_json::to_string(frame).expect("frame json");
    let body = kb_term::wire::s2c_body(viewer, json.as_bytes()).expect("s2c");
    kb_term::wire::encode_envelope(ENV_TEXT, &body).expect("env")
}

fn bin_env(viewer: &str, offset: u64, payload: &[u8]) -> Vec<u8> {
    let body = kb_term::wire::s2c_body(viewer, &kb_term::wire::encode_binary(offset, payload))
        .expect("s2c");
    kb_term::wire::encode_envelope(ENV_BINARY, &body).expect("env")
}

// ------------------------------------------------------------- fake runner --

#[derive(Default, Debug)]
struct RunnerLog {
    takes: Vec<(u16, u16, String, bool)>,
    keys: Vec<Vec<u8>>,
    launches: Vec<serde_json::Value>,
    opened_viewers: std::collections::HashSet<String>,
    /// Every request op seen, in order (for failure diagnostics).
    ops: Vec<String>,
}

struct FakeRunner {
    sock: PathBuf,
    log: Arc<Mutex<RunnerLog>>,
    stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeRunner {
    fn start(dir: &TempDir, session_list: serde_json::Value, take_failures: usize) -> FakeRunner {
        let sock = dir.file("runner/runner.sock");
        std::fs::create_dir_all(sock.parent().unwrap()).expect("mkdir runner dir");
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind runner");
        listener.set_nonblocking(true).expect("nonblocking");
        let log = Arc::new(Mutex::new(RunnerLog::default()));
        let take_failures = Arc::new(Mutex::new(take_failures));
        let log2 = log.clone();
        let failures2 = take_failures.clone();
        let list = session_list.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::spawn(move || loop {
            if stop2.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let conn = match listener.accept() {
                Ok((c, _)) => c,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(_) => return,
            };
            let reader = conn.try_clone().expect("clone conn");
            let log3 = log2.clone();
            let failures3 = failures2.clone();
            let list3 = list.clone();
            std::thread::spawn(move || {
                handle_runner_conn(
                    std::io::BufReader::new(reader),
                    conn,
                    &log3,
                    &failures3,
                    list3,
                );
            });
        });
        FakeRunner {
            sock,
            log,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn recorded_takes(&self) -> Vec<(u16, u16, String, bool)> {
        self.log.lock().unwrap().takes.clone()
    }
    fn recorded_keys(&self) -> Vec<Vec<u8>> {
        self.log.lock().unwrap().keys.clone()
    }
    fn recorded_launches(&self) -> Vec<serde_json::Value> {
        self.log.lock().unwrap().launches.clone()
    }
    fn recorded_opened(&self) -> std::collections::HashSet<String> {
        self.log.lock().unwrap().opened_viewers.clone()
    }
    fn recorded_ops(&self) -> Vec<String> {
        self.log.lock().unwrap().ops.clone()
    }
}

impl Drop for FakeRunner {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let _ = std::fs::remove_file(&self.sock);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn handle_runner_conn(
    mut reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    mut stream: std::os::unix::net::UnixStream,
    log: &Arc<Mutex<RunnerLog>>,
    take_failures: &Arc<Mutex<usize>>,
    session_list: serde_json::Value,
) {
    use std::io::BufRead as _;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let req: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                let resp =
                    r#"{"v":1,"id":null,"status":"err","code":"bad-request","message":"bad json"}"#;
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(b"\n");
                return;
            }
        };
        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("");
        log.lock().unwrap().ops.push(op.to_string());
        let mut resp = match op {
            "ping" => serde_json::json!({"status": "ok", "data": {"pong": true}}),
            "list" => serde_json::json!({"status": "ok", "data": session_list}),
            "launch" => {
                log.lock().unwrap().launches.push(req.clone());
                serde_json::json!({"status": "ok", "data": {"id": "newid1234", "pane_pid": 42, "cols": 120, "rows": 40}})
            }
            "take_size" => {
                let cols = req.get("cols").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                let rows = req.get("rows").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                let owner = req
                    .get("owner")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let force = req.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
                let mut f = take_failures.lock().unwrap();
                if *f > 0 {
                    *f -= 1;
                    serde_json::json!({"status": "err", "code": "grid-owned", "message": "grid is owned by local:999"})
                } else {
                    drop(f);
                    log.lock()
                        .unwrap()
                        .takes
                        .push((cols, rows, owner.clone(), force));
                    serde_json::json!({"status": "ok", "data": {"cols": cols, "rows": rows, "owner": owner}})
                }
            }
            "viewer_open" => {
                let viewer = req.get("viewer").and_then(|v| v.as_str()).unwrap_or("");
                log.lock()
                    .unwrap()
                    .opened_viewers
                    .insert(viewer.to_string());
                serde_json::json!({"status": "ok", "data": {"viewer": viewer}})
            }
            "viewer_close" => {
                let viewer = req.get("viewer").and_then(|v| v.as_str()).unwrap_or("");
                log.lock().unwrap().opened_viewers.remove(viewer);
                serde_json::json!({"status": "ok", "data": {"viewer": viewer}})
            }
            "keys" => {
                use base64::Engine as _;
                // The real runner rejects keys for a viewer that never
                // opened; the fake enforces the same `no-viewer` gate.
                let viewer = req.get("viewer").and_then(|v| v.as_str()).unwrap_or("");
                if !log.lock().unwrap().opened_viewers.contains(viewer) {
                    serde_json::json!({"status": "err", "code": "no-viewer", "message": "viewer not open"})
                } else {
                    // Key bytes live at req["input"]["bytes_b64"] (the
                    // kind-tagged KeyInput payload).
                    let b64 = req
                        .get("input")
                        .and_then(|i| i.get("bytes_b64"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(b64.as_bytes())
                        .unwrap_or_default();
                    log.lock().unwrap().keys.push(bytes);
                    serde_json::json!({"status": "ok", "data": {}})
                }
            }
            _ => {
                serde_json::json!({"status": "err", "code": "bad-request", "message": "unknown op"})
            }
        };
        resp["id"] = id;
        resp["v"] = serde_json::json!(1);
        let mut s = resp.to_string();
        s.push('\n');
        if stream.write_all(s.as_bytes()).is_err() {
            return;
        }
    }
}

// ----------------------------------------------------------------- fake tap --
#[derive(Clone)]

struct TapScript {
    cols: u16,
    rows: u16,
    session: String,
    /// Seed/live line counts (default 3000/300).
    seed_lines: u32,
    live_lines: u32,
    /// Per-connection drop points in stream bytes (total seed+live).
    drops: Vec<u64>,
    /// Insert a Clipboard frame mid-live.
    clipboard: bool,
    /// Send image defs.
    defs: bool,
    /// The fake sends the live chunking size.
    chunk: usize,
    /// Delay before the Exit frame (ms), for key/tap races.
    exit_delay_ms: u64,
}

struct FakeTap {
    sock: PathBuf,
    opens: Arc<Mutex<Vec<serde_json::Value>>>,
    controls: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl FakeTap {
    fn start(dir: &TempDir, script: TapScript) -> FakeTap {
        let sock = dir.file(&format!("s/{}/tap.sock", script.session));
        std::fs::create_dir_all(sock.parent().unwrap()).expect("mkdir tap dir");
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind tap");
        listener.set_nonblocking(true).expect("nonblocking");
        let opens = Arc::new(Mutex::new(Vec::new()));
        let controls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let opens2 = opens.clone();
        let controls2 = controls.clone();
        let stop2 = stop.clone();
        let thread = std::thread::spawn(move || {
            let mut conn_index = 0usize;
            let script = script.clone();
            loop {
                if stop2.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                let conn = match listener.accept() {
                    Ok((c, _)) => c,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => return,
                };
                let budget = script.drops.get(conn_index).copied().unwrap_or(u64::MAX);
                let opens3 = opens2.clone();
                let controls3 = controls2.clone();
                let s = script.clone();
                std::thread::spawn(move || {
                    handle_tap_conn(conn, &s, budget, &opens3, &controls3);
                });
                conn_index += 1;
            }
        });
        FakeTap {
            sock,
            opens,
            controls,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    fn recorded_opens(&self) -> Vec<serde_json::Value> {
        self.opens.lock().unwrap().clone()
    }
    fn wait_control(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self
                .controls
                .lock()
                .unwrap()
                .iter()
                .any(|c| c.get("t").and_then(|v| v.as_str()) == Some(needle))
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for FakeTap {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let _ = std::fs::remove_file(&self.sock);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn handle_tap_conn(
    mut stream: std::os::unix::net::UnixStream,
    script: &TapScript,
    drop_after: u64,
    opens: &Arc<Mutex<Vec<serde_json::Value>>>,
    controls: &Arc<Mutex<Vec<serde_json::Value>>>,
) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    // 1) The viewer-open control envelope.
    let mut decoder = EnvelopeDecoder::new();
    let mut buf = [0u8; 8192];
    let mut viewer: Option<String> = None;
    let mut resume: Option<serde_json::Value> = None;
    while viewer.is_none() {
        let n = match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        for env in decoder.push(&buf[..n]).unwrap_or_default() {
            if env.kind == ENV_CONTROL {
                let text = String::from_utf8_lossy(&env.body).into_owned();
                let v: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if v.get("t").and_then(|x| x.as_str()) == Some("viewer-open") {
                    viewer = v.get("viewer").and_then(|x| x.as_str()).map(str::to_string);
                    resume = v.get("resume").cloned();
                    opens.lock().unwrap().push(v);
                }
            }
        }
    }
    let viewer = viewer.unwrap();
    let (seed_bytes, live_bytes) =
        build_t4(script.cols as usize, script.seed_lines, script.live_lines);
    let total = (seed_bytes.len() + live_bytes.len()) as u64;
    let is_resume = resume.as_ref().is_some_and(|r| r.as_object().is_some());
    let resume_offset: u64 = resume
        .as_ref()
        .and_then(|r| r.get("offset"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut sent: u64 = 0; // stream bytes sent on THIS connection
    let write = |envs: &[Vec<u8>], stream: &mut std::os::unix::net::UnixStream| -> bool {
        for e in envs {
            if stream.write_all(e).is_err() {
                return false;
            }
        }
        true
    };
    // hello always.
    if !write(
        &[text_env(&viewer, &hello_frame(&script.session))],
        &mut stream,
    ) {
        return;
    }
    if is_resume {
        // Same-epoch resume: binary replay of the live bytes from the
        // resume offset onward (no seed, no ED3 — the client already
        // has its scrollback).
        let mut offset = resume_offset;
        for chunk in live_bytes.chunks(script.chunk) {
            let clen = chunk.len() as u64;
            if offset + clen <= resume_offset {
                offset += clen;
                continue;
            }
            let skip = (resume_offset - offset) as usize;
            let piece = &chunk[skip..];
            if !write(
                &[bin_env(&viewer, offset + skip as u64, piece)],
                &mut stream,
            ) {
                return;
            }
            sent += piece.len() as u64;
            offset += clen;
            if sent >= drop_after {
                drop_and_close(&mut stream);
                return;
            }
        }
        if resume_offset > live_bytes.len() as u64 {
            return;
        }
    } else {
        // Full join: profile, defs, seed, seed binaries (all header 0).
        let mut burst = vec![text_env(&viewer, &profile_frame(script.cols, script.rows))];
        if script.defs {
            for d in image_defs() {
                burst.push(text_env(&viewer, &d));
            }
        }
        burst.push(text_env(
            &viewer,
            &ServerFrame::Seed {
                offset: 0,
                len: seed_bytes.len() as u64,
            },
        ));
        if !write(&burst, &mut stream) {
            return;
        }
        for chunk in seed_bytes.chunks(65_536) {
            if !write(&[bin_env(&viewer, 0, chunk)], &mut stream) {
                return;
            }
            sent += chunk.len() as u64;
            if sent >= drop_after {
                drop_and_close(&mut stream);
                return;
            }
        }
        // Live bytes continue at offset 0 (applied == seed_x == 0).
        let mut offset: u64 = 0;
        for chunk in live_bytes.chunks(script.chunk) {
            if !write(&[bin_env(&viewer, offset, chunk)], &mut stream) {
                return;
            }
            sent += chunk.len() as u64;
            offset += chunk.len() as u64;
            if sent >= drop_after {
                drop_and_close(&mut stream);
                return;
            }
            if script.clipboard && offset >= 2_000 && offset - (chunk.len() as u64) < 2_000 {
                let _ = write(
                    &[text_env(
                        &viewer,
                        &ServerFrame::Clipboard {
                            offset,
                            data_b64: "aGVsbG8=".into(),
                        },
                    )],
                    &mut stream,
                );
            }
        }
    }
    let _ = total;
    // Optional hold before the Exit (typed/clipboard tests write a
    // key while the stream is still flowing): keep reading any
    // control envelopes (the typed notification) during the hold.
    if script.exit_delay_ms > 0 {
        let deadline = Instant::now() + Duration::from_millis(script.exit_delay_ms);
        stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .ok();
        while Instant::now() < deadline {
            let n = match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => continue,
            };
            for env in decoder.push(&buf[..n]).unwrap_or_default() {
                if env.kind == ENV_CONTROL {
                    let text = String::from_utf8_lossy(&env.body).into_owned();
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        controls.lock().unwrap().push(v);
                    }
                }
            }
        }
    }
    let _ = write(
        &[text_env(
            &viewer,
            &ServerFrame::Exit {
                seq: 9_999,
                code: Some(0),
                offset: 0,
            },
        )],
        &mut stream,
    );
    // Drain any trailing typed sent alongside the exit, then close.
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .ok();
    let n = stream.read(&mut buf);
    if let Ok(n) = n {
        for env in decoder.push(&buf[..n]).unwrap_or_default() {
            if env.kind == ENV_CONTROL {
                let text = String::from_utf8_lossy(&env.body).into_owned();
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    controls.lock().unwrap().push(v);
                }
            }
        }
    }
}

fn drop_and_close(stream: &mut std::os::unix::net::UnixStream) {
    // Let the client buffer drain so the drop lands at a
    // deterministic stream position, then cut the socket.
    std::thread::sleep(Duration::from_millis(80));
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn t4_seed_lines() -> u32 {
    3000
}
fn t4_live_lines() -> u32 {
    300
}

// --------------------------------------------------------------- pty bits --

struct PtyProc {
    master: std::fs::File,
    child: Child,
    reader_rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
}

fn bin_path() -> String {
    std::env::var("CARGO_BIN_EXE_kb-term").expect("CARGO_BIN_EXE_kb-term")
}

fn spawn_bin(args: &[&str], cols: u16, rows: u16) -> (PtyProc, std::fs::File) {
    let (master, slave) =
        kb_term::pty::open_pty(cols, rows, cols * 9, rows * 18).expect("open pty");
    let child = Command::new(bin_path())
        .args(args)
        .stdin(Stdio::from(std::fs::File::from(
            slave.try_clone().expect("dup slave"),
        )))
        .stdout(Stdio::from(std::fs::File::from(
            slave.try_clone().expect("dup slave"),
        )))
        .stderr(Stdio::from(std::fs::File::from(
            slave.try_clone().expect("dup slave"),
        )))
        .spawn()
        .expect("spawn kb-term");
    drop(slave);
    let master_read = master.try_clone().expect("dup master");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut m = std::fs::File::from(master_read);
        let mut out = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            match m.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                // `open_pty` sets the master O_NONBLOCK: a momentarily
                // empty read is EAGAIN, not EOF — retry, or the child
                // blocks forever once the pty buffer fills (it can
                // never exit a test).
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(out);
    });
    let master_write = std::fs::File::from(master);
    let stdin_end = master_write.try_clone().expect("clone master");
    (
        PtyProc {
            master: master_write,
            child,
            reader_rx: Some(rx),
        },
        stdin_end,
    )
}

impl PtyProc {
    fn wait_for_exit(&mut self, timeout: Duration) -> (Option<i32>, Vec<u8>) {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    std::thread::sleep(Duration::from_millis(50));
                    let out = self
                        .reader_rx
                        .take()
                        .and_then(|rx| rx.recv_timeout(Duration::from_secs(2)).ok())
                        .unwrap_or_default();
                    return (status.code(), out);
                }
                Ok(None) => {
                    if Instant::now() > deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        let out = self
                            .reader_rx
                            .take()
                            .and_then(|rx| rx.recv_timeout(Duration::from_secs(2)).ok())
                            .unwrap_or_default();
                        let text = String::from_utf8_lossy(&out).into_owned();
                        let tail = &text[text.len().saturating_sub(2000)..];
                        panic!(
                            "child did not exit within {timeout:?}; captured {} bytes \
                             (session-ended: {}, reconnecting: {}, ed3: {}), tail: {tail:?}",
                            out.len(),
                            text.contains("[session ended"),
                            text.contains("reconnecting"),
                            text.matches("\x1b[3J").count()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => {
                    let _ = self.child.kill();
                    return (None, Vec::new());
                }
            }
        }
    }
}

// ------------------------------------------------------------- dump logic --

fn strip_csis(line: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        if line[i] == 0x1B && i + 1 < line.len() && line[i + 1] == b'[' {
            let mut j = i + 2;
            while j < line.len() && !(0x40..=0x7E).contains(&line[j]) {
                j += 1;
            }
            i = j.saturating_add(1);
            continue;
        }
        out.push(line[i]);
        i += 1;
    }
    out
}

/// Port of the spike `NUM_RE`: `^(?:\x1b\[[0-9;?]*[A-Za-z])*L(\d{6})\b`.
fn num_of(line: &[u8]) -> Option<u32> {
    let stripped = strip_csis(line);
    if stripped.len() < 7 || stripped[0] != b'L' {
        return None;
    }
    let digits = &stripped[1..7];
    if !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if stripped.len() > 7 {
        let after = stripped[7];
        if after.is_ascii_alphanumeric() || after == b'_' {
            return None;
        }
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

#[derive(Debug)]
struct ModelReport {
    ed3_count: usize,
    sync_ok: bool,
    segment_ok: bool,
    last_is_1_to_n: bool,
    highest: u32,
    final_lines: Vec<String>,
}

fn model(capture: &[u8], expect_total: u32) -> ModelReport {
    let text = String::from_utf8_lossy(capture).into_owned();
    let ed3_count = text.matches("\x1b[3J").count();
    // The terminal's exit restore also emits `ESC[?2026l`: one extra
    // close without a matching open.
    let sync_ok = text.matches("\x1b[?2026h").count() + 1 == text.matches("\x1b[?2026l").count();
    let parts: Vec<&str> = text.split("\x1b[3J").collect();
    let mut segment_ok = true;
    let mut last_numbers: Vec<u32> = Vec::new();
    let mut final_lines: Vec<String> = Vec::new();
    for (idx, seg) in parts.iter().enumerate().skip(1) {
        let mut nums: Vec<u32> = Vec::new();
        let mut lines: Vec<String> = Vec::new();
        for raw in seg.split("\r\n") {
            if let Some(n) = num_of(raw.as_bytes()) {
                nums.push(n);
                lines.push(raw.to_string());
            }
        }
        if nums != (1..=nums.len() as u32).collect::<Vec<_>>() {
            segment_ok = false;
        }
        if idx == parts.len() - 1 {
            last_numbers = nums;
            final_lines = lines;
        }
    }
    let highest = *last_numbers.iter().max().unwrap_or(&0);
    let last_is_1_to_n = last_numbers == (1..=expect_total).collect::<Vec<_>>();
    ModelReport {
        ed3_count,
        sync_ok,
        segment_ok,
        last_is_1_to_n,
        highest,
        final_lines,
    }
}

fn count_occurrences(out: &[u8], needle: &[u8]) -> usize {
    out.windows(needle.len()).filter(|w| *w == needle).count()
}

fn run_python_checkdump(lines: &[String], expect_total: u32) {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let script = manifest.join("tools/mirror_standin.py");
    let dump_dir = TempDir::new();
    let dump = dump_dir.file("dump.txt");
    let mut content = String::new();
    for l in lines {
        content.push_str(l);
        content.push('\n');
    }
    std::fs::write(&dump, content).expect("write dump");
    match Command::new("python3")
        .args([
            "-I",
            &script.to_string_lossy(),
            "--check-dump",
            &dump.to_string_lossy(),
            "--expect-total",
            &expect_total.to_string(),
        ])
        .stdin(Stdio::null())
        .output()
    {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "python check-dump must pass: {text}");
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("python3 not found; skipping T0 check-dump ({e})");
        }
        Err(e) => panic!("python3 spawn: {e}"),
    }
}

// ------------------------------------------------------------------- setup --

fn session_list() -> serde_json::Value {
    serde_json::json!([{
        "id": "abcd1234", "harness": "omp", "cwd": "/tmp", "started_unix": 1,
        "resume_sid": "deadbeef", "pane_pid": 123, "pane_dead": false,
        "exit_code": null, "cols": 201, "rows": 48,
        "bracket_paste_flag": false, "grid_owner": null,
        "conf_version": 2, "anchor_pid": 7, "stale": false
    }])
}

struct Rig {
    _dir: TempDir,
    runner: FakeRunner,
    tap: FakeTap,
    sock_str: String,
}

fn setup_rig(take_failures: usize, script: TapScript) -> Rig {
    let dir = TempDir::new();
    assert_eq!(script.session, "abcd1234");
    std::fs::create_dir_all(dir.file("s/abcd1234")).expect("mkdir session dir");
    let runner = FakeRunner::start(&dir, session_list(), take_failures);
    let tap = FakeTap::start(&dir, script);
    let sock_str = runner.sock.to_string_lossy().into_owned();
    Rig {
        _dir: dir,
        runner,
        tap,
        sock_str,
    }
}

fn t4_script() -> TapScript {
    TapScript {
        cols: 201,
        rows: 48,
        session: "abcd1234".into(),
        seed_lines: t4_seed_lines(),
        live_lines: t4_live_lines(),
        drops: vec![],
        clipboard: false,
        defs: true,
        chunk: 65_536,
        exit_delay_ms: 0,
    }
}

// ------------------------------------------------------------------- tests --

#[test]
fn attach_t4_seed_then_3_rebuilds_last_segment_1_to_n_once() {
    if !term_int_gate() {
        return;
    }
    let mut script = t4_script();
    // Hold the Exit so a keystroke can be written while the attach is
    // live; the keys assertion then exercises a real forward.
    script.exit_delay_ms = 3_000;
    let rig = setup_rig(0, script);
    std::thread::sleep(Duration::from_millis(100));
    let mut proc = spawn_bin(
        &["attach", "abcd1234", "--runner-sock", &rig.sock_str],
        201,
        48,
    );
    std::thread::sleep(Duration::from_millis(1_500));
    proc.1.write_all(b"T").expect("write key");
    let (code, out) = proc.0.wait_for_exit(Duration::from_secs(25));
    let report = model(&out, 3300);
    assert_eq!(code, Some(0), "normal exit");
    assert_eq!(report.ed3_count, 4, "mirror's ED3 + 3 rebuilds");
    assert!(report.sync_ok, "balanced ?2026");
    assert!(report.segment_ok, "every segment is 1..len consecutive");
    assert!(report.last_is_1_to_n, "last segment is 1..3300");
    assert!(
        out.windows(PLACEHOLDER.len()).all(|w| w != PLACEHOLDER),
        "zero U+10EEEE"
    );
    let labels = count_occurrences(&out, b"on web]");
    assert!(labels >= 66, "a label per 50th line, got {labels}");
    assert!(
        out.windows(b"[session ended: code 0]".len())
            .any(|w| w == b"[session ended: code 0]"),
        "exit prints the code"
    );
    assert!(
        out.windows(b"\x1b[?2026l".len())
            .any(|w| w == b"\x1b[?2026l"),
        "restore sequence"
    );
    assert!(
        !rig.runner.recorded_takes().is_empty(),
        "attach took the grid"
    );
    assert_eq!(rig.runner.recorded_takes().len(), 1);
    assert!(
        !rig.runner.recorded_opened().is_empty(),
        "the client opened the viewer (hidden-client keys need it); ops: {:?}",
        rig.runner.recorded_ops()
    );
    assert_eq!(
        rig.runner.recorded_keys(),
        vec![b"T".to_vec()],
        "the written key was forwarded to the hidden client"
    );
    run_python_checkdump(&report.final_lines, 3300);
}
#[test]
fn attach_t4_truncated_capture_is_flagged() {
    if !term_int_gate() {
        return;
    }
    // The truncation check runs on the ACTUAL capture of a real attach
    // (a small stream for speed), cut at 90%: the model MUST flag it.
    let mut script = t4_script();
    script.seed_lines = 300;
    script.live_lines = 30;
    let rig = setup_rig(0, script);
    std::thread::sleep(Duration::from_millis(100));
    let mut proc = spawn_bin(
        &["attach", "abcd1234", "--runner-sock", &rig.sock_str],
        201,
        48,
    );
    let (code, out) = proc.0.wait_for_exit(Duration::from_secs(15));
    assert_eq!(code, Some(0));
    assert!(model(&out, 330).last_is_1_to_n, "full capture is 1..330");
    let cut = (out.len() as f64 * 0.9) as usize;
    let report = model(&out[..cut], 330);
    assert!(
        report.highest < 330,
        "truncated capture flagged (highest {} < 330)",
        report.highest
    );
    assert!(!report.last_is_1_to_n);
}

#[test]
fn attach_four_forced_drops_equals_undropped() {
    if !term_int_gate() {
        return;
    }
    // Undropped run.
    let rig_u = setup_rig(0, t4_script());
    std::thread::sleep(Duration::from_millis(100));
    let mut proc_u = spawn_bin(
        &["attach", "abcd1234", "--runner-sock", &rig_u.sock_str],
        201,
        48,
    );
    let (code_u, out_u) = proc_u.0.wait_for_exit(Duration::from_secs(20));
    assert_eq!(code_u, Some(0));
    let report_u = model(&out_u, 3300);
    assert!(report_u.last_is_1_to_n);
    // Dropped run: 4 mid-stream drops, ALL past the seed (budgets are
    // stream bytes per connection, and the seed alone exceeds the old
    // budget). A drop mid-seed would force a full rejoin with a fresh
    // ED3, so the offsets are kept well past the seed's end.
    let mut script = t4_script();
    script.drops = vec![400_000, 30_000, 30_000, 30_000];
    let rig_d = setup_rig(0, script);
    std::thread::sleep(Duration::from_millis(100));
    let mut proc_d = spawn_bin(
        &["attach", "abcd1234", "--runner-sock", &rig_d.sock_str],
        201,
        48,
    );
    let (code_d, out_d) = proc_d.0.wait_for_exit(Duration::from_secs(30));
    assert_eq!(
        code_d,
        Some(0),
        "dropped run exits 0; ops: {:?}; tail: {:?}",
        rig_d.runner.recorded_ops(),
        String::from_utf8_lossy(&out_d[out_d.len().saturating_sub(1500)..])
    );
    let report_d = model(&out_d, 3300);
    assert!(report_d.last_is_1_to_n);
    // Reconnects interleave stderr progress lines on the pty, so byte
    // equality cannot hold; the MODEL must match exactly.
    assert_eq!(report_d.ed3_count, report_u.ed3_count, "same ED3 count");
    assert_eq!(
        report_d.final_lines, report_u.final_lines,
        "model content equals the undropped run"
    );
    // The reconnects exercised ResumeFrom (at least one resume open).
    assert!(
        rig_d
            .tap
            .recorded_opens()
            .iter()
            .any(|o| o.get("resume").is_some_and(|r| !r.is_null())),
        "resume was requested after drops"
    );
}

#[test]
fn attach_watch_sends_no_takesize_and_reads_no_stdin() {
    if !term_int_gate() {
        return;
    }
    let mut script = t4_script();
    script.exit_delay_ms = 2_000;
    let rig = setup_rig(0, script);
    std::thread::sleep(Duration::from_millis(100));
    let mut proc = spawn_bin(
        &[
            "attach",
            "abcd1234",
            "--watch",
            "--runner-sock",
            &rig.sock_str,
        ],
        201,
        48,
    );
    // stdin bytes ARE written mid-run; a watch must still never read
    // them (no keys, no take-size), and the stream still completes.
    std::thread::sleep(Duration::from_millis(1_000));
    proc.1.write_all(b"w").expect("write stdin in watch");
    let (code, out) = proc.0.wait_for_exit(Duration::from_secs(20));
    assert_eq!(code, Some(0));
    assert!(
        rig.runner.recorded_takes().is_empty(),
        "watch sends no take-size"
    );
    assert!(rig.runner.recorded_keys().is_empty(), "watch sends no keys");
    let report = model(&out, 3300);
    assert!(report.last_is_1_to_n);
}

#[test]
fn attach_grid_owned_prints_banner_and_watches_until_key() {
    if !term_int_gate() {
        return;
    }
    let mut script = t4_script();
    script.exit_delay_ms = 3_000;
    let rig = setup_rig(1, script);
    std::thread::sleep(Duration::from_millis(100));
    let mut proc = spawn_bin(
        &["attach", "abcd1234", "--runner-sock", &rig.sock_str],
        201,
        48,
    );
    std::thread::sleep(Duration::from_millis(1_500));
    proc.1.write_all(b"k").expect("take key");
    let (code, out) = proc.0.wait_for_exit(Duration::from_secs(20));
    assert_eq!(code, Some(0));
    assert!(
        out.windows(b"grid owned by another kb-term attach".len())
            .any(|w| w == b"grid owned by another kb-term attach"),
        "fallback banner (stderr merged onto the pty)"
    );
    let takes = rig.runner.recorded_takes();
    // Two takes: the forced take the key triggers, and the normal
    // take the restart join does (spec §6: on success the attach
    // restarts at steps 3-5, whose take-size succeeds).
    assert_eq!(takes.len(), 2, "forced take + restart take");
    assert!(takes[0].3, "the first take was forced (any key)");
    assert!(!takes[1].3, "the restart take is a normal take-size");
    assert!(
        rig.runner.recorded_keys().is_empty(),
        "the take key was NOT forwarded as keys"
    );
    // The fallback watched: stream output reached the terminal before
    // the key was pressed (spec §6 keeps pumping in watch mode).
    assert!(
        out.windows(b"on web]".len()).any(|w| w == b"on web]"),
        "picture labels shown during the fallback watch"
    );
}

#[test]
fn attach_clipboard_and_typed_notified() {
    if !term_int_gate() {
        return;
    }
    let mut script = t4_script();
    script.clipboard = true;
    script.exit_delay_ms = 3_000;
    let rig = setup_rig(0, script);
    std::thread::sleep(Duration::from_millis(100));
    let mut proc = spawn_bin(
        &["attach", "abcd1234", "--runner-sock", &rig.sock_str],
        201,
        48,
    );
    std::thread::sleep(Duration::from_millis(1_500));
    proc.1.write_all(b"x").expect("key");
    let (code, out) = proc.0.wait_for_exit(Duration::from_secs(20));
    assert_eq!(code, Some(0));
    assert_eq!(rig.runner.recorded_keys(), vec![b"x".to_vec()]);
    assert!(
        rig.tap.wait_control("typed", Duration::from_secs(3)),
        "typed notification sent after keys"
    );
    assert!(
        out.windows(b"\x1b]52;c;aGVsbG8=".len())
            .any(|w| w == b"\x1b]52;c;aGVsbG8="),
        "the Clipboard frame is forwarded as local OSC 52"
    );
}

// ------------------------------------------------------------ CLI smoke tests

/// Spawn and wait for the binary with a hard deadline (no unbounded
/// `Command::output()` in this file: a hang must fail the test, not
/// burn the lane timeout).
fn wait_output(args: &[&str]) -> std::process::Output {
    let mut child = Command::new(bin_path())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bin");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("bin {args:?} did not exit within 15s");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("bin {args:?} wait: {e}"),
        }
    }
    child.wait_with_output().expect("wait_with_output")
}

#[test]
fn exit_code_3_when_runner_unreachable() {
    if !term_int_gate() {
        return;
    }
    let out = wait_output(&[
        "ls",
        "--runner-sock",
        "/tmp/kbt-nope-xyz/runner/runner.sock",
    ]);
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("kb-termd user unit may not be running"),
        "{err}"
    );
}

#[test]
fn exit_code_4_when_session_missing() {
    if !term_int_gate() {
        return;
    }
    // Session not found -> exit 4.
    let dir = TempDir::new();
    let runner = FakeRunner::start(&dir, serde_json::json!([]), 0);
    let sock = runner.sock.to_string_lossy().into_owned();
    let out = wait_output(&["attach", "abcd1234", "--runner-sock", &sock]);
    assert_eq!(out.status.code(), Some(4));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("session abcd1234 not found"), "{err}");
}

#[test]
fn ls_json_roundtrips_runner_rows() {
    if !term_int_gate() {
        return;
    }
    let list = serde_json::json!([
        {
            "id": "abcd1234", "harness": "omp", "cols": 201, "rows": 48,
            "pane_dead": false, "stale": false, "grid_owner": "local:55",
            "resume_sid": "deadbeef", "exit_code": null
        },
        {
            "id": "efgh5678", "harness": "claude", "cols": 80, "rows": 24,
            "pane_dead": true, "stale": false, "grid_owner": null,
            "resume_sid": null, "exit_code": 2
        },
        {
            "id": "ijkl9012", "harness": null, "cols": null, "rows": null,
            "pane_dead": false, "stale": true, "grid_owner": null,
            "resume_sid": null, "exit_code": null
        }
    ]);
    let dir = TempDir::new();
    let runner = FakeRunner::start(&dir, list.clone(), 0);
    let sock = runner.sock.to_string_lossy().into_owned();
    let out = wait_output(&["ls", "--runner-sock", &sock]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    // The grid owner `local:55` is a DIFFERENT local attach (the child
    // pid): it renders `other`, never `self`.
    assert!(
        text.contains("abcd1234 omp 201x48 running other deadbeef"),
        "{text}"
    );
    assert!(
        text.contains("efgh5678 claude 80x24 exited 2 none -"),
        "{text}"
    );
    assert!(text.contains("ijkl9012"), "{text}");
    // --json prints the raw runner rows: parse them back and compare.
    let out = wait_output(&["ls", "--json", "--runner-sock", &sock]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&text).expect("ls --json must be valid JSON");
    assert_eq!(parsed, list, "ls --json roundtrips the runner rows");
}

#[test]
fn new_rejects_unknown_harness() {
    if !term_int_gate() {
        return;
    }
    let dir = TempDir::new();
    let runner = FakeRunner::start(&dir, serde_json::json!([]), 0);
    let sock = runner.sock.to_string_lossy().into_owned();
    let out = wait_output(&["new", "badharness", "--no-attach", "--runner-sock", &sock]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "unknown harness is a usage error"
    );
    assert!(
        runner.recorded_launches().is_empty(),
        "no launch must reach the runner"
    );
}

#[test]
fn new_launches_omp_and_prints_id() {
    if !term_int_gate() {
        return;
    }
    let dir = TempDir::new();
    let runner = FakeRunner::start(&dir, serde_json::json!([]), 0);
    let sock = runner.sock.to_string_lossy().into_owned();
    let out = wait_output(&["new", "omp", "--no-attach", "--runner-sock", &sock]);
    assert_eq!(out.status.code(), Some(0), "new must complete");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("newid1234"), "new prints the id: {text}");
    let launches = runner.recorded_launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(launches[0]["harness"], "omp");
    assert_eq!(launches[0]["argv"][0], "/usr/local/bin/omp");
}

#[test]
fn new_passes_resume_cwd_and_size() {
    if !term_int_gate() {
        return;
    }
    let dir = TempDir::new();
    let runner = FakeRunner::start(&dir, serde_json::json!([]), 0);
    let sock = runner.sock.to_string_lossy().into_owned();
    let workdir = dir.file("work");
    std::fs::create_dir_all(&workdir).expect("mkdir work");
    let out = wait_output(&[
        "new",
        "omp",
        "--resume",
        "deadbeef",
        "--cwd",
        &workdir.to_string_lossy(),
        "--cols",
        "120",
        "--rows",
        "40",
        "--no-attach",
        "--runner-sock",
        &sock,
    ]);
    assert_eq!(out.status.code(), Some(0));
    let launches = runner.recorded_launches();
    assert_eq!(launches.len(), 1);
    let l = &launches[0];
    assert_eq!(l["harness"], "omp");
    assert_eq!(l["resume_sid"], "deadbeef", "resume passthrough");
    assert_eq!(
        l["cwd"],
        workdir.to_string_lossy().into_owned(),
        "cwd passthrough"
    );
    assert_eq!(l["cols"], 120, "cols passthrough");
    assert_eq!(l["rows"], 40, "rows passthrough");
}

#[test]
fn new_bad_cwd_exits_2() {
    if !term_int_gate() {
        return;
    }
    let dir = TempDir::new();
    let runner = FakeRunner::start(&dir, serde_json::json!([]), 0);
    let sock = runner.sock.to_string_lossy().into_owned();
    let out = wait_output(&[
        "new",
        "omp",
        "--cwd",
        "/nonexistent-kbt-dir",
        "--no-attach",
        "--runner-sock",
        &sock,
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a bad cwd is a usage failure (exit 2), not an internal error (1)"
    );
    assert!(
        runner.recorded_launches().is_empty(),
        "no launch must reach the runner"
    );
}

#[test]
fn new_cols_bounds_exit_2() {
    if !term_int_gate() {
        return;
    }
    let dir = TempDir::new();
    let runner = FakeRunner::start(&dir, serde_json::json!([]), 0);
    let sock = runner.sock.to_string_lossy().into_owned();
    let out = wait_output(&[
        "new",
        "omp",
        "--cols",
        "600",
        "--no-attach",
        "--runner-sock",
        &sock,
    ]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "cols outside 20..=500 is a usage failure"
    );
    assert!(runner.recorded_launches().is_empty());
}

#[test]
fn no_subcommand_exits_2_with_usage() {
    if !term_int_gate() {
        return;
    }
    let out = wait_output(&[]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("missing subcommand"),
        "usage wording (no scaffold jargon): {err}"
    );
}

#[test]
fn attach_size_mismatch_prints_banner() {
    if !term_int_gate() {
        return;
    }
    // The tap profile is 201x48; a 120x40 pane must print the one-line
    // stderr banner before the seed.
    let rig = setup_rig(0, t4_script());
    std::thread::sleep(Duration::from_millis(100));
    let mut proc = spawn_bin(
        &["attach", "abcd1234", "--runner-sock", &rig.sock_str],
        120,
        40,
    );
    let (code, out) = proc.0.wait_for_exit(Duration::from_secs(25));
    assert_eq!(code, Some(0));
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("kb-term: grid size mismatch: session 201x48, local pane is 120x40"),
        "mismatch banner (stderr merged onto the pty): {text}"
    );
}

#[test]
fn attach_sigwinch_retakes_once_debounced() {
    if !term_int_gate() {
        return;
    }
    // Two quick SIGWINCHs must produce exactly ONE extra take-size
    // (100 ms debounce): the initial take plus one debounced take.
    let mut script = t4_script();
    script.seed_lines = 300;
    script.live_lines = 30;
    script.exit_delay_ms = 3_000;
    let rig = setup_rig(0, script);
    std::thread::sleep(Duration::from_millis(100));
    let mut proc = spawn_bin(
        &["attach", "abcd1234", "--runner-sock", &rig.sock_str],
        201,
        48,
    );
    std::thread::sleep(Duration::from_millis(1_500));
    let pid = proc.0.child.id();
    // Resize the pty so the new size differs from the last take.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        ws.ws_col = 100;
        ws.ws_row = 24;
        libc::ioctl(proc.0.master.as_raw_fd(), libc::TIOCSWINSZ, &ws);
    }
    kill_pid(pid, 28); // SIGWINCH
    std::thread::sleep(Duration::from_millis(30));
    kill_pid(pid, 28);
    let (code, _out) = proc.0.wait_for_exit(Duration::from_secs(25));
    assert_eq!(code, Some(0));
    let takes = rig.runner.recorded_takes();
    assert_eq!(
        takes.len(),
        2,
        "initial take + one debounced SIGWINCH take: {takes:?}"
    );
}
fn kill_pid(pid: u32, sig: i32) {
    unsafe {
        libc::kill(pid as i32, sig);
    }
}
