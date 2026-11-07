//! term-int integration tests for the T5 tap twin + reseed (tests 16-25).
//!
//! These drive real tmux 3.7c (`KB_TMUX_BIN`) through the TS driver
//! (`tap/test/int/reseed-driver.ts`, run under Bun). With `KB_TMUX_BIN`
//! unset they early-return (pass) so the `workspace-test` lane stays green;
//! with `KB_REQUIRE_TMUX=1` an absent tmux FAILS (same shape as hook-omp's
//! `KB_REQUIRE_BUN=1`). Every pty/socket read has a hard deadline. All
//! fixtures are synthetic generators from `crates/kb-term/fixtures` or bytes
//! built in this file.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn tmux_bin() -> Option<PathBuf> {
    let require = std::env::var("KB_REQUIRE_TMUX").is_ok_and(|v| v == "1");
    match std::env::var("KB_TMUX_BIN") {
        Ok(v) if !v.is_empty() && Path::new(&v).exists() => Some(PathBuf::from(v)),
        _ => {
            if require {
                panic!("KB_REQUIRE_TMUX=1 but KB_TMUX_BIN is unset or missing");
            }
            None
        }
    }
}

fn tap_dir() -> PathBuf {
    // Cargo sets CARGO_MANIFEST_DIR to the kb-term crate dir for this test.
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn bun_bin() -> String {
    std::env::var("KB_BUN_BIN").unwrap_or_else(|_| "bun".to_string())
}

/// A unique temp directory, removed on drop. Std-only (no new deps).
struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("kbt-t5-{}-{tag}-{n}", std::process::id()));
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

/// One private tmux server with the brief's session options.
struct Server {
    tmux: PathBuf,
    sock: PathBuf,
    _dir: TempDir,
}

impl Server {
    fn start(tmux: &Path) -> Server {
        let dir = TempDir::new("srv");
        let sock = dir.file("s.sock");
        let srv = Server {
            tmux: tmux.to_path_buf(),
            sock,
            _dir: dir,
        };
        srv
    }

    fn run(&self, session: &str, sub: &[&str]) -> Vec<u8> {
        let mut argv: Vec<std::ffi::OsString> = vec![
            std::ffi::OsString::from(&self.tmux),
            std::ffi::OsString::from("-S"),
            self.sock.clone().into_os_string(),
            std::ffi::OsString::from("-f"),
            std::ffi::OsString::from("/dev/null"),
        ];
        if !session.is_empty() {
            argv.push(std::ffi::OsString::from("-t"));
            argv.push(std::ffi::OsString::from(session));
        }
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .unwrap_or_else(|e| panic!("tmux {sub:?} spawn: {e}"));
        if !out.status.success() {
            panic!(
                "tmux {sub:?} failed: status={} stdout={} stderr={}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        out.stdout
    }

    fn tmux(&self, args: &[&str]) -> Vec<u8> {
        self.run("", args)
    }

    fn server_pid(&self) -> u32 {
        let s = String::from_utf8_lossy(&self.tmux(&["display-message", "-p", "#{pid}"]))
            .trim()
            .to_string();
        s.parse()
            .unwrap_or_else(|_| panic!("bad server pid output: {s:?}"))
    }

    /// Create the brief's session: manual window size, remain-on-exit, and the
    /// pane command gated on a ready-file so the driver is attached BEFORE the
    /// stream starts. Returns the pane id (%N).
    fn new_session(
        &self,
        name: &str,
        cols: u32,
        rows: u32,
        ready: &Path,
        payload: &[&str],
    ) -> String {
        let gate = format!("while [ ! -e \"$1\" ]; do sleep 0.05; done; shift; exec \"$@\"",);
        let mut argv: Vec<String> =
            vec!["new-session".into(), "-d".into(), "-s".into(), name.into()];
        argv.push("-x".into());
        argv.push(cols.to_string());
        argv.push("-y".into());
        argv.push(rows.to_string());
        argv.push("-P".into());
        argv.push("-F".into());
        argv.push("#{pane_id}".into());
        argv.push("sh".into());
        argv.push("-c".into());
        argv.push(gate);
        argv.push("_".into());
        argv.push(ready.to_string_lossy().into_owned());
        argv.extend(payload.iter().map(|s| s.to_string()));
        let refs: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
        let pane = String::from_utf8_lossy(&self.tmux(&refs))
            .trim()
            .to_string();
        assert!(pane.starts_with('%'), "unexpected pane id: {pane:?}");
        self.tmux(&["set", "-g", "window-size", "manual"]);
        self.tmux(&["set", "-g", "remain-on-exit", "on"]);
        self.tmux(&["set", "-g", "exit-empty", "off"]);
        self.tmux(&["set", "-g", "history-limit", "50000"]);
        self.tmux(&["set", "history-limit", "50000"]);
        self.tmux(&["set", "-g", "default-terminal", "tmux-256color"]);
        self.tmux(&["set", "-g", "allow-passthrough", "off"]);
        pane
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new(&self.tmux)
            .args([
                "-S",
                &self.sock.to_string_lossy(),
                "-f",
                "/dev/null",
                "kill-server",
            ])
            .output();
    }
}

fn rss_bytes(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Run the TS driver to completion with a hard deadline; returns its JSON
/// (last stdout line) and stderr tail for panic messages.
fn run_driver(
    sock: &Path,
    session: &str,
    tmux: &Path,
    cols: u32,
    rows: u32,
    pane: &str,
    ready: &Path,
    ref_mode: &str,
    stream: &Path,
    scenario: &str,
    expect_bytes: usize,
    quiescent: Option<&str>,
    deadline_secs: u64,
) -> (serde_json::Value, String) {
    let driver = tap_dir().join("tap/test/int/reseed-driver.ts");
    let mut cmd = Command::new(bun_bin());
    for a in [
        "run".to_string(),
        driver.to_string_lossy().into_owned(),
        "--socket".into(),
        sock.to_string_lossy().into_owned(),
        "--session".into(),
        session.to_string(),
        "--tmux".into(),
        tmux.to_string_lossy().into_owned(),
        "--cols".into(),
        cols.to_string(),
        "--rows".into(),
        rows.to_string(),
        "--pane".into(),
        pane.to_string(),
        "--ready".into(),
        ready.to_string_lossy().into_owned(),
        "--ref".into(),
        ref_mode.to_string(),
        "--stream".into(),
        stream.to_string_lossy().into_owned(),
        "--scenario".into(),
        scenario.to_string(),
        "--expect-bytes".into(),
        expect_bytes.to_string(),
    ] {
        cmd.arg(a);
    }
    if let Some(q) = quiescent {
        cmd.arg("--quiescent").arg(q);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn driver: {e}"));
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("driver for {scenario} exceeded {deadline_secs}s");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("driver wait: {e}"),
        }
    }
    let out = child.wait_with_output().expect("driver output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let last_line = stdout.lines().last().unwrap_or("").to_string();
    let json: serde_json::Value = match serde_json::from_str(&last_line) {
        Ok(v) => v,
        Err(_) => {
            panic!(
        "driver JSON unparseable. last line={last_line:?}\nstdout={stdout}\nstderr={stderr}"
      );
        }
    };
    (json, stderr)
}

/// Small inline helpers that fail with the driver stderr tail.
fn assert_json_ok(json: &serde_json::Value, stderr: &str) {
    assert_eq!(
        json.get("ok"),
        Some(&serde_json::Value::Bool(true)),
        "driver not ok: {json}\nstderr: {stderr}"
    );
}

fn gen_stream(tag: &str, args: &[&str]) -> (TempDir, PathBuf) {
    let dir = TempDir::new(tag);
    let out = dir.file("stream.bin");
    let gen = tap_dir().join("fixtures/fake_harness.py");
    let status = Command::new("python3")
        .arg("-I")
        .arg(gen)
        .args(args)
        .arg("-o")
        .arg(&out)
        .status()
        .unwrap_or_else(|e| panic!("fake_harness spawn: {e}"));
    assert!(status.success(), "fake_harness failed: {status}");
    (dir, out)
}

/// Feed the stream slowly (~12 s total) so reseeds land mid-stream.
const FEED_PY: &str = "import sys,time\n\
d=open(sys.argv[1],'rb').read()\n\
n=8192\n\
k=max(1,len(d)//n)\n\
delay=12.0/k\n\
o=sys.stdout.buffer\n\
for i in range(0,len(d),n):\n\
    o.write(d[i:i+n])\n\
    o.flush()\n\
    time.sleep(delay)\n\
o.flush()\n\
time.sleep(300)\n";

#[test]
fn term_int_reseed_equals_tmux_for_text_workload() {
    let Some(tmux) = tmux_bin() else { return };
    let (gdir, stream) = gen_stream(
        "text",
        &[
            "--seed", "7", "--cols", "100", "--rows", "30", "--length", "800",
        ],
    );
    let srv = Server::start(&tmux);
    let feed = srv._dir.file("feed.py");
    std::fs::write(&feed, FEED_PY).expect("write feed.py");
    let ready = srv._dir.file("ready");
    let pane = srv.new_session(
        "s",
        100,
        30,
        &ready,
        &[
            "python3",
            "-I",
            feed.to_str().unwrap(),
            stream.to_str().unwrap(),
        ],
    );
    let (json, stderr) = run_driver(
        &srv.sock, "s", &tmux, 100, 30, &pane, &ready, "emitted", &stream, "text", 1, None, 120,
    );
    assert_json_ok(&json, &stderr);
    let comps = json
        .get("comparisons")
        .and_then(|c| c.as_array())
        .expect("comparisons");
    assert_eq!(
        comps.len(),
        12,
        "expected 12 reconstructions, got {}",
        comps.len()
    );
    for (i, c) in comps.iter().enumerate() {
        assert_eq!(
            c.get("ok"),
            Some(&serde_json::Value::Bool(true)),
            "comparison {i} not ok: {c}"
        );
    }
    let _ = gdir;
}

#[test]
fn term_int_ten_concurrent_reseeds() {
    let Some(tmux) = tmux_bin() else { return };
    let (gdir, stream) = gen_stream(
        "conc",
        &[
            "--seed", "11", "--cols", "100", "--rows", "30", "--length", "60",
        ],
    );
    let srv = Server::start(&tmux);
    let stream_len = std::fs::metadata(&stream).map(|m| m.len()).unwrap_or(0) as usize;
    // One shared pane running the whole stream.
    let ready = srv._dir.file("ready");
    let pane = srv.new_session("s", 100, 30, &ready, &["cat", stream.to_str().unwrap()]);
    // 10 staggered driver processes, each with its own control client + twin.
    let mut hashes: Vec<String> = Vec::new();
    let mut children: Vec<Child> = Vec::new();
    let driver = tap_dir().join("tap/test/int/reseed-driver.ts");
    for i in 0..10 {
        let mut cmd = Command::new(bun_bin());
        for a in [
            "run",
            driver.to_str().unwrap(),
            "--socket",
            srv.sock.to_str().unwrap(),
            "--session",
            "s",
            "--tmux",
            tmux.to_str().unwrap(),
            "--cols",
            "100",
            "--rows",
            "30",
            "--pane",
            pane.as_str(),
            "--ready",
            ready.to_str().unwrap(),
            "--ref",
            "file",
            "--stream",
            stream.to_str().unwrap(),
            "--scenario",
            "concurrent",
            "--expect-bytes",
            &stream_len.to_string(),
        ] {
            cmd.arg(a);
        }
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn driver");
        children.push(child);
        if i < 9 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    for mut child in children {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        panic!("concurrent driver exceeded 120s");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("concurrent driver wait: {e}"),
            }
        }
        let out = child.wait_with_output().expect("driver output");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let json: serde_json::Value = serde_json::from_str(stdout.lines().last().unwrap_or("{}"))
            .unwrap_or_else(|_| panic!("driver JSON unparseable: {stdout}"));
        assert_json_ok(&json, &String::from_utf8_lossy(&out.stderr));
        hashes.push(
            json.get("compare")
                .and_then(|c| c.get("hash"))
                .and_then(|h| h.as_str())
                .unwrap_or("")
                .to_string(),
        );
    }
    assert_eq!(hashes.len(), 10);
    for h in &hashes[1..] {
        assert_eq!(
            h, &hashes[0],
            "concurrent reseeds must reconstruct identically: {hashes:?}"
        );
    }
    let _ = gdir;
}

#[test]
fn term_int_reseed_after_long_pause() {
    let Some(tmux) = tmux_bin() else { return };
    let srv = Server::start(&tmux);
    // 30 MB flood file: repeated copy of one synthetic line.
    let flood = srv._dir.file("flood.bin");
    let line = b"KBTERM-FLOOD-0123456789\r\n";
    let reps = 30_000_000 / line.len() + 1;
    let mut bytes = Vec::with_capacity(reps * line.len());
    for _ in 0..reps {
        bytes.extend_from_slice(line);
    }
    bytes.truncate(30_000_000);
    std::fs::write(&flood, &bytes).expect("write flood");
    let ready = srv._dir.file("ready");
    let pane = srv.new_session("s", 100, 30, &ready, &["cat", flood.to_str().unwrap()]);
    let pid = srv.server_pid();
    // Run the driver (pause-after=2, stall, continue, reseed) and poll RSS.
    let driver = tap_dir().join("tap/test/int/reseed-driver.ts");
    let mut cmd = Command::new(bun_bin());
    for a in [
        "run",
        driver.to_str().unwrap(),
        "--socket",
        srv.sock.to_str().unwrap(),
        "--session",
        "s",
        "--tmux",
        tmux.to_str().unwrap(),
        "--cols",
        "100",
        "--rows",
        "30",
        "--pane",
        pane.as_str(),
        "--ready",
        ready.to_str().unwrap(),
        "--ref",
        "file",
        "--stream",
        flood.to_str().unwrap(),
        "--scenario",
        "pause",
    ] {
        cmd.arg(a);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn driver");
    let mut max_rss: u64 = 0;
    let mut stalled = false;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if ready.exists() {
            stalled = true;
        }
        if stalled {
            if let Some(r) = rss_bytes(pid) {
                max_rss = max_rss.max(r);
            }
        }
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    panic!("pause driver exceeded 120s");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("pause driver wait: {e}"),
        }
    }
    let out = child.wait_with_output().expect("driver output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        max_rss > 0 && max_rss < 100 * 1024 * 1024,
        "tmux RSS during 30MB flood must stay under 100MB, observed peak {max_rss} bytes"
    );
    let json: serde_json::Value = serde_json::from_str(stdout.lines().last().unwrap_or("{}"))
        .unwrap_or_else(|_| panic!("driver JSON unparseable: {stdout}\nstderr: {stderr}"));
    assert_json_ok(&json, &stderr);
}

#[test]
fn term_int_reseed_mid_line_cursor() {
    let Some(tmux) = tmux_bin() else { return };
    let srv = Server::start(&tmux);
    let stream = srv._dir.file("stream.bin");
    std::fs::write(&stream, b"HELLO\r\nWORLD").expect("write stream");
    let ready = srv._dir.file("ready");
    let pane = srv.new_session("s", 100, 30, &ready, &["cat", stream.to_str().unwrap()]);
    let (json, stderr) = run_driver(
        &srv.sock, "s", &tmux, 100, 30, &pane, &ready, "file", &stream, "cursor", 12, None, 90,
    );
    assert_json_ok(&json, &stderr);
    let c = &json["compare"];
    assert_eq!(
        c["cursor"]["x"],
        serde_json::json!(5),
        "cursor x must be mid-line 5: {c}"
    );
    assert_eq!(
        c["cursor"]["y"],
        serde_json::json!(1),
        "cursor y must be 1: {c}"
    );
    assert_eq!(
        c["cursor"], c["cursorRef"],
        "reseeded cursor must equal reference: {c}"
    );
}

#[test]
fn term_int_reseed_pending_wrap() {
    let Some(tmux) = tmux_bin() else { return };
    let srv = Server::start(&tmux);
    let stream = srv._dir.file("stream.bin");
    let mut bytes = Vec::with_capacity(100);
    bytes.extend(std::iter::repeat(b'X').take(100));
    std::fs::write(&stream, &bytes).expect("write stream");
    let ready = srv._dir.file("ready");
    let pane = srv.new_session("s", 100, 30, &ready, &["cat", stream.to_str().unwrap()]);
    let (json, stderr) = run_driver(
        &srv.sock, "s", &tmux, 100, 30, &pane, &ready, "file", &stream, "wrap", 100, None, 90,
    );
    assert_json_ok(&json, &stderr);
    let c = &json["compare"];
    // Cells must match. Cursor state: either tmux exposes the pending wrap and
    // the twin mirrors it exactly, or the diff is only the pending-wrap /
    // cursor-position class (the recorded documented residual).
    assert_eq!(
        c["cellsDiff"],
        serde_json::json!(0),
        "cells must match: {c}"
    );
    let classes = c
        .get("stateDiffClasses")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let allowed = ["pending wrap", "cursor position"];
    for class in &classes {
        assert!(
            allowed.contains(&class.as_str().unwrap_or("")),
            "unexpected state diff class {class:?}; allowed: {allowed:?}; full: {c}"
        );
    }
}

#[test]
fn term_int_reseed_wrapped_lines_and_blank_tail() {
    let Some(tmux) = tmux_bin() else { return };
    let srv = Server::start(&tmux);
    let stream = srv._dir.file("stream.bin");
    let mut bytes = Vec::new();
    // Four lines longer than the 100-col pane (they wrap), then a short line.
    for row in 0..4 {
        let ch = b'A' + (row as u8);
        bytes.extend(std::iter::repeat(ch).take(250));
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(b"END\r\n");
    std::fs::write(&stream, &bytes).expect("write stream");
    let ready = srv._dir.file("ready");
    let pane = srv.new_session("s", 100, 30, &ready, &["cat", stream.to_str().unwrap()]);
    let len = bytes.len();
    let (json, stderr) = run_driver(
        &srv.sock, "s", &tmux, 100, 30, &pane, &ready, "file", &stream, "wrapped", len, None, 90,
    );
    assert_json_ok(&json, &stderr);
    assert_eq!(
        json["compare"]["cellsDiff"],
        serde_json::json!(0),
        "wrapped content must equal reference"
    );
}

#[test]
fn term_int_reseed_alt_screen() {
    let Some(tmux) = tmux_bin() else { return };
    let srv = Server::start(&tmux);
    let stream = srv._dir.file("stream.bin");
    std::fs::write(
        &stream,
        b"NORMAL-ONE\r\nNORMAL-TWO\r\n\x1b[?1049hALT-ONE\r\nALT-TWO\r\n",
    )
    .expect("write stream");
    let ready = srv._dir.file("ready");
    let pane = srv.new_session("s", 100, 30, &ready, &["cat", stream.to_str().unwrap()]);
    let (json, stderr) = run_driver(
        &srv.sock, "s", &tmux, 100, 30, &pane, &ready, "file", &stream, "alt", 40, None, 90,
    );
    assert_json_ok(&json, &stderr);
    let c = &json["compare"];
    assert_eq!(
        c["cellsDiff"],
        serde_json::json!(0),
        "alt-screen reseed must equal reference: {c}"
    );
    assert_eq!(
        c["altCellsDiff"],
        serde_json::json!(0),
        "alt buffer must equal reference: {c}"
    );
}

#[test]
fn term_int_mode_formats_exist_on_pinned_tmux() {
    let Some(tmux) = tmux_bin() else { return };
    let srv = Server::start(&tmux);
    let ready = srv._dir.file("ready");
    let pane = srv.new_session("s", 100, 30, &ready, &["sleep", "300"]);
    let out = srv.run("s", &["display-message", "-a", "-t", &pane]);
    let text = String::from_utf8_lossy(&out);
    // Every name in reseed.ts's MODE_FORMATS must be listed by display-message -a.
    for name in [
        "alternate_on",
        "scroll_region_upper",
        "scroll_region_lower",
        "wrap_flag",
        "cursor_flag",
        "insert_flag",
        "keypad_cursor_flag",
        "keypad_flag",
        "origin_flag",
    ] {
        assert!(
            text.contains(name),
            "format {name} missing from display-message -a output"
        );
    }
    let _ = pane;
}

#[test]
fn term_int_placeholder_rows_survive_capture_pane() {
    let Some(tmux) = tmux_bin() else { return };
    let srv = Server::start(&tmux);
    // Synthetic omp kitty stream (placeholder rows, SGR truecolour + diacritics).
    let gen = tap_dir().join("fixtures/synth_omp_kitty_stream.py");
    let stream = srv._dir.file("stream.bin");
    let status = Command::new("python3")
        .arg("-I")
        .arg(gen)
        .args(["--cols", "100", "--rows", "19"])
        .stdout(std::fs::File::create(&stream).expect("create stream"))
        .status()
        .expect("synth_omp_kitty_stream spawn");
    assert!(status.success(), "synth stream failed");
    let ready = srv._dir.file("ready");
    let pane = srv.new_session("s", 100, 30, &ready, &["cat", stream.to_str().unwrap()]);
    let len = std::fs::metadata(&stream).map(|m| m.len()).unwrap_or(0) as usize;
    let (json, stderr) = run_driver(
        &srv.sock,
        "s",
        &tmux,
        100,
        30,
        &pane,
        &ready,
        "file",
        &stream,
        "placeholder",
        len,
        None,
        90,
    );
    assert_json_ok(&json, &stderr);
    assert_eq!(
        json["compare"]["cellsDiff"],
        serde_json::json!(0),
        "placeholder cells (char incl. diacritics, fg RGB) must survive capture-pane: {}",
        json["compare"]
    );
}

#[test]
fn term_int_partial_sequence_quiescent_wait() {
    let Some(tmux) = tmux_bin() else { return };
    let srv = Server::start(&tmux);
    let midseq = tap_dir().join("fixtures/midseq.py");
    let stream = srv._dir.file("stream.bin");
    // Full byte count: piece1 (14) + piece2 (29) + piece3 (27) = 70 decoded bytes.
    let _ = std::fs::write(&stream, b"");
    let ready = srv._dir.file("ready");
    let pane = srv.new_session(
        "s",
        100,
        30,
        &ready,
        &["python3", "-I", midseq.to_str().unwrap(), "--gap", "1"],
    );
    // ON case: both quiescent joins must be clean.
    let (json, stderr) = run_driver(
        &srv.sock,
        "s",
        &tmux,
        100,
        30,
        &pane,
        &ready,
        "emitted",
        &stream,
        "midseq",
        70,
        Some("on"),
        90,
    );
    assert_json_ok(&json, &stderr);
    assert_eq!(
        json["metric"],
        serde_json::json!({"csiMomentsOk": true, "oscMomentsOk": true, "finalOk": true}),
        "quiescent-ON joins must be clean: {json}"
    );
    // OFF case (metric only): restart the pane, reseed without the wait, and
    // RECORD the result in the test output as a metric -- never asserted.
    srv.run("s", &["kill-session", "-t", "s"]);
    let pane2 = srv.new_session(
        "s",
        100,
        30,
        &ready,
        &["python3", "-I", midseq.to_str().unwrap(), "--gap", "1"],
    );
    let _ = std::fs::remove_file(&ready);
    let (json2, stderr2) = run_driver(
        &srv.sock,
        "s",
        &tmux,
        100,
        30,
        &pane2,
        &ready,
        "emitted",
        &stream,
        "midseq",
        70,
        Some("off"),
        90,
    );
    eprintln!(
        "term_int_partial_sequence_quiescent_wait OFF metric: {}",
        json2["metric"]
    );
    let _ = stderr2;
}
