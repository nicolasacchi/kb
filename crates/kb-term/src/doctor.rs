//! `kb-term doctor` — host-local read-only diagnostics for the terminal
//! substrate. Shape mirrors `kb doctor` (status + id + detail + fix, with
//! pure `decide_*` fns fed by probe structs) but with its own exit rule:
//! default exit 1 iff any FAIL; `--strict` exit 1 iff any WARN or FAIL;
//! SKIP never fails.
//!
//! Std-only for metadata (no libc: the current uid comes from
//! `/proc/self` via `MetadataExt`), subprocess timeouts by spawn +
//! `try_wait` poll loop + kill. Never prints env values, tokens or
//! credential paths: details carry names/versions/paths only.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Check status. Mirrors the kb-cli doctor vocabulary (re-declared here
/// because kb-term has no kb-cli edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Pass,
    Warn,
    Skip,
    Fail,
}

impl CheckStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Pass => "pass",
            CheckStatus::Warn => "warn",
            CheckStatus::Skip => "skip",
            CheckStatus::Fail => "fail",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            CheckStatus::Pass => "✓",
            CheckStatus::Warn => "⚠",
            CheckStatus::Skip => "○",
            CheckStatus::Fail => "✗",
        }
    }
}

/// One check result: stable snake-case id, status, one-line detail, and
/// a one-line fix when there is one.
#[derive(Debug, Clone)]
pub struct Check {
    pub id: String,
    pub status: CheckStatus,
    pub detail: String,
    pub fix: Option<String>,
}

impl Check {
    pub fn new(id: impl Into<String>, status: CheckStatus, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            status,
            detail: detail.into(),
            fix: None,
        }
    }
    pub fn pass(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Pass, detail)
    }
    pub fn warn(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Warn, detail)
    }
    pub fn skip(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Skip, detail)
    }
    pub fn fail(id: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::new(id, CheckStatus::Fail, detail)
    }
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// The pinned xterm.js pins the tap must be built against.
pub const PINS: &[(&str, &str)] = &[
    ("@xterm/xterm", "6.0.0"),
    ("@xterm/headless", "6.0.0"),
    ("@xterm/addon-serialize", "0.14.0"),
    ("@xterm/addon-unicode-graphemes", "0.4.0"),
    ("@xterm/addon-image", "0.9.0"),
    ("@xterm/addon-webgl", "0.19.0"),
];

/// Known-good harness versions (kimi/grok unreconciled: never warned).
pub const KNOWN_GOOD: &[(&str, &str)] = &[
    ("omp", "18.17.0"),
    ("claude", "2.1.300"),
    ("codex", "0.149.0"),
    ("opencode", "1.17.20"),
];

pub const RUN_LAYOUT_ROOT: &str = "/run/kb-term";

/// The exit code for a check set: default 1 iff any FAIL; strict 1 iff
/// any WARN or FAIL; SKIP never fails.
pub fn exit_code(checks: &[Check], strict: bool) -> i32 {
    let mut code = 0;
    for c in checks {
        match c.status {
            CheckStatus::Fail => code = 1,
            CheckStatus::Warn => {
                if strict {
                    code = 1;
                }
            }
            _ => {}
        }
    }
    code
}

pub fn render_human(checks: &[Check]) -> String {
    let mut out = String::from("kb-term doctor — host-local diagnostics\n\n");
    for c in checks {
        out.push_str(&format!(
            "  {} {:<22} {}\n",
            c.status.glyph(),
            c.id,
            c.detail
        ));
        if let Some(fix) = &c.fix {
            out.push_str(&format!("    fix: {fix}\n"));
        }
    }
    out
}

pub fn to_json(checks: &[Check]) -> String {
    let items: Vec<serde_json::Value> = checks
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "status": c.status.as_str(),
                "detail": c.detail,
                "fix": c.fix,
            })
        })
        .collect();
    serde_json::to_string_pretty(&items).expect("check JSON serialises")
}

// ---------------------------------------------------------------- probes

/// Run `exe args` with an allowlisted env and a hard deadline:
/// `None` on timeout/spawn failure (the caller maps it to FAIL/WARN).
pub fn run_with_timeout(
    exe: &Path,
    args: &[&str],
    env: &[(String, String)],
    timeout: Duration,
) -> Option<(bool, String, String)> {
    let mut child = Command::new(exe)
        .args(args)
        .env_clear()
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child.wait_with_output().ok()?;
                return Some((
                    status.success(),
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                ));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

/// First non-empty output line, trimmed.
fn first_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn current_env() -> Vec<(String, String)> {
    std::env::vars().collect()
}

fn allowlisted_env() -> Vec<(String, String)> {
    crate::profiles::env::filter_env(&current_env())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The pinned tmux: `$KB_TERM_TMUX`, else `~/.local/libexec/kb-term/tmux`
/// (never a PATH tmux).
pub fn tmux_bin_path() -> PathBuf {
    if let Ok(v) = std::env::var("KB_TERM_TMUX") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    home_dir()
        .unwrap_or_default()
        .join(".local/libexec/kb-term/tmux")
}

/// `tmux -V` output, or `None` when missing/not executable/timeout.
pub fn probe_tmux_version(path: &Path) -> Option<String> {
    if !is_executable(path) {
        return None;
    }
    let (ok, stdout, _stderr) =
        run_with_timeout(path, &["-V"], &allowlisted_env(), Duration::from_secs(3))?;
    if ok {
        first_line(&stdout)
    } else {
        None
    }
}

pub fn decide_tmux_version(output: Option<&str>) -> Check {
    match output {
        None => Check::fail("tmux_version", "tmux binary missing or not executable")
            .with_fix("install the pinned tmux under ~/.local/libexec/kb-term/"),
        Some(v) if v.trim() == "tmux 3.7c" => Check::pass("tmux_version", "tmux 3.7c"),
        Some(v) => Check::warn(
            "tmux_version",
            format!("tmux reports `{v}`, not the pinned 3.7c"),
        ),
    }
}

/// (stream SHA-256 hex digest, sidecar first token) for `path`.
pub fn probe_tmux_sha(path: &Path) -> (Option<String>, Option<String>) {
    let digest = std::fs::File::open(path).ok().map(|mut f| {
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(_) => break,
            }
        }
        hex::encode(hasher.finalize())
    });
    let sidecar = std::fs::read_to_string(format!("{}.sha256", path.display()))
        .ok()
        .and_then(|s| s.split_whitespace().next().map(str::to_string));
    (digest, sidecar)
}

pub fn decide_tmux_sha256(digest: Option<&str>, sidecar: Option<&str>) -> Check {
    match (digest, sidecar) {
        (None, _) => Check::fail("tmux_sha256", "cannot hash the tmux binary"),
        (Some(_), None) => Check::warn(
            "tmux_sha256",
            "no <tmux>.sha256 sidecar to verify the binary against",
        ),
        (Some(d), Some(s)) if d.eq_ignore_ascii_case(s) => {
            Check::pass("tmux_sha256", "binary digest matches the sidecar")
        }
        (Some(_), Some(_)) => {
            Check::fail("tmux_sha256", "binary digest does not match the sidecar")
                .with_fix("re-download the pinned tmux; the sidecar is authoritative")
        }
    }
}

/// The tap binary: `$KB_TERM_TAP`, else `kb-tap` next to this exe.
pub fn tap_bin_path() -> PathBuf {
    if let Ok(v) = std::env::var("KB_TERM_TAP") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("kb-tap")))
        .unwrap_or_else(|| PathBuf::from("kb-tap"))
}

pub fn probe_tap_version(path: &Path) -> Option<String> {
    if !is_executable(path) {
        return None;
    }
    let (ok, stdout, _) = run_with_timeout(
        path,
        &["--version"],
        &allowlisted_env(),
        Duration::from_secs(3),
    )?;
    if ok {
        first_line(&stdout)
    } else {
        None
    }
}

pub fn decide_tap_binary(found: bool, version_line: Option<&str>) -> Check {
    if !found {
        return Check::fail("tap_binary", "kb-tap missing or not executable")
            .with_fix("build/install kb-tap next to kb-termd (or set KB_TERM_TAP)");
    }
    match version_line {
        Some(v) => Check::pass("tap_binary", format!("kb-tap at the recorded path ({v})")),
        None => Check::pass("tap_binary", "kb-tap executable (--version gave no output)"),
    }
}

/// `kb-tap --pins` JSON stdout when it exits zero.
pub fn probe_tap_pins(path: &Path) -> Option<String> {
    if !is_executable(path) {
        return None;
    }
    let (ok, stdout, _) = run_with_timeout(
        path,
        &["--pins"],
        &allowlisted_env(),
        Duration::from_secs(3),
    )?;
    if ok {
        Some(stdout)
    } else {
        None
    }
}

/// Parse the `--pins` JSON shapes (object map, or list of
/// `{name,version}` / `[name,version]`) into (name, version) pairs.
pub fn parse_pins(json: &str) -> Option<Vec<(String, String)>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    match v {
        serde_json::Value::Object(map) => Some(
            map.into_iter()
                .filter_map(|(k, vv)| vv.as_str().map(|s| (k, s.to_string())))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_json::Value::Object(o) => {
                        let name = o.get("name")?.as_str()?.to_string();
                        let version = o.get("version")?.as_str()?.to_string();
                        out.push((name, version));
                    }
                    serde_json::Value::Array(pair) if pair.len() == 2 => {
                        let name = pair[0].as_str()?.to_string();
                        let version = pair[1].as_str()?.to_string();
                        out.push((name, version));
                    }
                    _ => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

pub fn decide_xterm_pins(pins_output: Option<&str>) -> Check {
    let Some(raw) = pins_output else {
        return Check::skip("xterm_pins", "tap does not report pins");
    };
    let Some(reported) = parse_pins(raw) else {
        return Check::skip("xterm_pins", "tap does not report pins");
    };
    let mut missing: Vec<String> = Vec::new();
    for (name, want) in PINS {
        match reported.iter().find(|(n, _)| n == name) {
            Some((_, got)) if got == want => {}
            Some((_, got)) => missing.push(format!("{name} {got} != {want}")),
            None => missing.push(format!("{name} missing (want {want})")),
        }
    }
    if missing.is_empty() {
        Check::pass("xterm_pins", "tap reports the pinned xterm set")
    } else {
        Check::fail(
            "xterm_pins",
            format!("tap pin drift: {}", missing.join("; ")),
        )
    }
}

/// Resolve a harness binary on the allowlisted PATH plus `~/.local/bin`.
pub fn harness_bin_path(name: &str) -> Option<PathBuf> {
    let raw = current_env();
    if let Some((_, path)) = raw.iter().find(|(k, _)| k == "PATH") {
        for part in path.split(':') {
            if part.is_empty() {
                continue;
            }
            let cand = PathBuf::from(part).join(name);
            if is_executable(&cand) {
                return Some(cand);
            }
        }
    }
    if let Some(home) = home_dir() {
        let cand = home.join(".local/bin").join(name);
        if is_executable(&cand) {
            return Some(cand);
        }
    }
    None
}

pub fn probe_harness_version(path: &Path) -> Option<String> {
    let (ok, stdout, stderr) = run_with_timeout(
        path,
        &["--version"],
        &allowlisted_env(),
        Duration::from_secs(5),
    )?;
    if !ok {
        return None;
    }
    first_line(&stdout).or_else(|| first_line(&stderr))
}

pub fn decide_harness(
    name: &str,
    version_line: Option<&str>,
    known_good: Option<&str>,
    experimental: bool,
) -> Check {
    let id = format!("harness_{name}");
    let Some(v) = version_line else {
        return Check::skip(id, "not found on PATH or ~/.local/bin");
    };
    let mut detail = format!("{name} at {v}");
    if experimental {
        detail.push_str(" (experimental profile)");
    }
    match known_good {
        None => Check::pass(
            id,
            format!("{detail} (version unreconciled, recorded not gated)"),
        ),
        Some(want) if v.contains(want) => Check::pass(id, detail),
        Some(want) => Check::warn(id, format!("{detail} (known good {want})")),
    }
}

/// The shipped omp overlay: `$KB_TERM_SHARE/omp/overlay.yml`, else
/// `<exe>/../share/kb-term/omp/overlay.yml`.
pub fn overlay_path() -> PathBuf {
    if let Ok(v) = std::env::var("KB_TERM_SHARE") {
        if !v.is_empty() {
            return PathBuf::from(v).join("omp").join("overlay.yml");
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|e| {
            e.parent()
                .map(|p| p.join("../share/kb-term/omp/overlay.yml"))
        })
        .unwrap_or_else(|| PathBuf::from("share/kb-term/omp/overlay.yml"))
}

/// Parse the on-disk overlay by indentation: `maxInlineImages: 0` and
/// `resizeScrollback: rebuild` under `tui:`, `collapseCompacted: false`
/// under `display:`. Returns the missing/different keys.
pub fn overlay_missing_keys(content: &str) -> Vec<&'static str> {
    // Keys are unique across the two sections, so a flat name map is
    // enough for the by-indentation check.
    let mut seen: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            let v = v.split('#').next().unwrap_or(v).trim();
            seen.insert(k.trim(), v.to_string());
        }
    }
    let mut missing = Vec::new();
    if seen.get("maxInlineImages").map(String::as_str) != Some("0") {
        missing.push("tui.maxInlineImages != 0");
    }
    if seen.get("resizeScrollback").map(String::as_str) != Some("rebuild") {
        missing.push("tui.resizeScrollback != rebuild");
    }
    if seen.get("collapseCompacted").map(String::as_str) != Some("false") {
        missing.push("display.collapseCompacted != false");
    }
    missing
}

pub fn decide_omp_overlay(content: Option<&str>) -> Check {
    let Some(raw) = content else {
        return Check::skip(
            "omp_overlay",
            "no on-disk overlay (only the embedded copy exists)",
        );
    };
    let missing = overlay_missing_keys(raw);
    if missing.is_empty() {
        Check::pass("omp_overlay", "on-disk overlay carries the approved pins")
    } else {
        Check::fail(
            "omp_overlay",
            format!("on-disk overlay wrong: {}", missing.join("; ")),
        )
    }
}

/// (mode, uid) for a path, if it exists.
#[derive(Debug, Clone, Copy)]
pub struct DirState {
    pub mode: u32,
    pub uid: u32,
}

#[derive(Debug, Clone)]
pub struct RunLayoutProbe {
    pub root: Option<DirState>,
    pub sessions: Option<DirState>,
    pub runner: Option<DirState>,
    pub sock: Option<DirState>,
    pub cur_uid: u32,
}

fn dir_state(path: &Path) -> Option<DirState> {
    std::fs::metadata(path).ok().map(|m| DirState {
        mode: m.permissions().mode() & 0o777,
        uid: m.uid(),
    })
}

pub fn decide_run_layout(probe: &RunLayoutProbe) -> Check {
    let id = "run_layout";
    let tmpfiles_hint = "sudo systemd-tmpfiles --create";
    let Some(root) = probe.root else {
        return Check::fail(id, "run layout missing").with_fix(tmpfiles_hint);
    };
    let Some(sessions) = probe.sessions else {
        return Check::fail(id, "run layout missing s/").with_fix(tmpfiles_hint);
    };
    let Some(runner) = probe.runner else {
        return Check::fail(id, "run layout missing runner/").with_fix(tmpfiles_hint);
    };
    let mut problems: Vec<String> = Vec::new();
    if root.mode != 0o755 {
        problems.push(format!("/run/kb-term mode {:04o} want 0755", root.mode));
    }
    if sessions.mode != 0o700 {
        problems.push(format!(
            "/run/kb-term/s mode {:04o} want 0700",
            sessions.mode
        ));
    }
    if !(runner.mode == 0o750 || runner.mode == 0o700) {
        problems.push(format!(
            "/run/kb-term/runner mode {:04o} want 0750 or 0700",
            runner.mode
        ));
    }
    if let Some(sock) = probe.sock {
        if sock.mode != 0o600 {
            problems.push(format!("runner.sock mode {:04o} want 0600", sock.mode));
        }
    }
    for (what, st) in [("run", &root), ("s/", &sessions), ("runner/", &runner)] {
        if st.uid != probe.cur_uid {
            problems.push(format!("{what} owned by wrong uid"));
        }
    }
    if let Some(sock) = probe.sock {
        if sock.uid != probe.cur_uid {
            problems.push("runner.sock owned by wrong uid".to_string());
        }
    }
    if problems.is_empty() {
        Check::pass(id, "run layout modes and ownership are right")
    } else {
        Check::fail(id, problems.join("; ")).with_fix(tmpfiles_hint)
    }
}

fn probe_run_layout() -> RunLayoutProbe {
    let root = Path::new(RUN_LAYOUT_ROOT);
    let cur_uid = std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .unwrap_or(0);
    RunLayoutProbe {
        root: dir_state(root),
        sessions: dir_state(&root.join("s")),
        runner: dir_state(&root.join("runner")),
        sock: dir_state(&root.join("runner.sock")),
        cur_uid,
    }
}

/// `$SHELL -lc env` names: `None` when `$SHELL` is unset or the shell
/// cannot run.
pub fn probe_login_env(shell: &str) -> Option<Vec<String>> {
    let (ok, stdout, _) = run_with_timeout(
        Path::new(shell),
        &["-lc", "env"],
        &allowlisted_env(),
        Duration::from_secs(5),
    )?;
    if !ok {
        return None;
    }
    Some(
        stdout
            .lines()
            .filter_map(|l| l.split('=').next())
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect(),
    )
}

pub const POSIX_SHELLS: &[&str] = &["bash", "zsh", "sh", "dash", "ksh"];

pub fn decide_login_env(shell: Option<&str>, forbidden: &[String], shell_ok: bool) -> Check {
    let id = "login_env";
    let Some(shell) = shell else {
        return Check::skip(id, "$SHELL unset");
    };
    let mut notes: Vec<String> = Vec::new();
    if !forbidden.is_empty() {
        notes.push(format!(
            "login shell reintroduces named vars: {}",
            forbidden.join(", ")
        ));
    }
    if !shell_ok {
        notes.push(format!(
            "shell {shell} is not POSIX (wrapper is POSIX-only)"
        ));
    }
    if notes.is_empty() {
        Check::pass(id, "login env is clean")
    } else {
        Check::warn(id, notes.join("; "))
    }
}

// ---------------------------------------------------------------- driver

/// Run every check against the real host state.
pub fn run_checks() -> Vec<Check> {
    let mut checks = Vec::new();
    // tmux
    let tmux = tmux_bin_path();
    let version = probe_tmux_version(&tmux);
    checks.push(decide_tmux_version(version.as_deref()));
    let (digest, sidecar) = probe_tmux_sha(&tmux);
    checks.push(decide_tmux_sha256(digest.as_deref(), sidecar.as_deref()));
    // tap
    let tap = tap_bin_path();
    let tap_found = is_executable(&tap);
    let tver = if tap_found {
        probe_tap_version(&tap)
    } else {
        None
    };
    checks.push(decide_tap_binary(tap_found, tver.as_deref()));
    let pins = if tap_found {
        probe_tap_pins(&tap)
    } else {
        None
    };
    checks.push(decide_xterm_pins(pins.as_deref()));
    // harnesses
    for name in crate::profiles::HARNESSES {
        let path = harness_bin_path(name);
        let vline = path.as_ref().and_then(|p| probe_harness_version(p));
        let known_good = KNOWN_GOOD.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
        let experimental = crate::profiles::table::row(name)
            .map(|r| r.experimental)
            .unwrap_or(false);
        checks.push(decide_harness(
            name,
            vline.as_deref(),
            known_good,
            experimental,
        ));
    }
    // omp overlay
    let overlay = overlay_path();
    let content = std::fs::read_to_string(&overlay).ok();
    checks.push(decide_omp_overlay(content.as_deref()));
    // run layout
    checks.push(decide_run_layout(&probe_run_layout()));
    // login env
    match std::env::var("SHELL").ok() {
        None => checks.push(decide_login_env(None, &[], false)),
        Some(shell) => {
            let names = probe_login_env(&shell).unwrap_or_default();
            let forbidden: Vec<String> = names
                .iter()
                .filter(|n| crate::profiles::env::is_forbidden_name(n.as_str()))
                .cloned()
                .collect();
            let base = Path::new(&shell)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let shell_ok = POSIX_SHELLS.contains(&base.as_str());
            checks.push(decide_login_env(Some(shell.as_str()), &forbidden, shell_ok));
        }
    }
    checks
}

/// Run doctor and print the report; returns the process exit code.
pub fn run(json: bool, strict: bool) -> i32 {
    let checks = run_checks();
    if json {
        println!("{}", to_json(&checks));
    } else {
        print!("{}", render_human(&checks));
    }
    exit_code(&checks, strict)
}
