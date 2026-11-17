//! Doctor tests: one per `decide_*` with PASS/WARN/FAIL/SKIP inputs plus
//! JSON hygiene and exit-code rules. Version probes use fake executables
//! written to a tempdir.

use kb_term::doctor::{self, CheckStatus, DirState, RunLayoutProbe};
use std::os::unix::fs::PermissionsExt;

fn fake_exe(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn doctor_tmux_version() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_exe(dir.path(), "tmux", "#!/bin/sh\necho 'tmux 3.7c'\n");
    let out = doctor::probe_tmux_version(&fake).unwrap();
    let check = doctor::decide_tmux_version(Some(&out));
    assert_eq!(check.status, CheckStatus::Pass);
    assert_eq!(check.id, "tmux_version");
    // Wrong version warns; missing fails.
    let warn = doctor::decide_tmux_version(Some("tmux 3.4"));
    assert_eq!(warn.status, CheckStatus::Warn);
    let fail = doctor::decide_tmux_version(None);
    assert_eq!(fail.status, CheckStatus::Fail);
}

#[test]
fn doctor_tmux_sha256() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_exe(dir.path(), "tmux", "#!/bin/sh\necho 'tmux 3.7c'\n");
    let (digest, sidecar) = doctor::probe_tmux_sha(&fake);
    assert!(digest.is_some());
    assert!(sidecar.is_none());
    let check = doctor::decide_tmux_sha256(digest.as_deref(), sidecar.as_deref());
    assert_eq!(check.status, CheckStatus::Warn); // no sidecar
    let good = digest.clone().unwrap();
    let ok = doctor::decide_tmux_sha256(Some(&good), Some(&good));
    assert_eq!(ok.status, CheckStatus::Pass);
    let bad = doctor::decide_tmux_sha256(Some(&good), Some("00"));
    assert_eq!(bad.status, CheckStatus::Fail);
    let missing = doctor::decide_tmux_sha256(None, None);
    assert_eq!(missing.status, CheckStatus::Fail);
}

#[test]
fn doctor_tap_binary() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_exe(dir.path(), "kb-tap", "#!/bin/sh\necho 'kb-tap 0.1.0'\n");
    let line = doctor::probe_tap_version(&fake).unwrap();
    let check = doctor::decide_tap_binary(true, Some(&line));
    assert_eq!(check.status, CheckStatus::Pass);
    let check = doctor::decide_tap_binary(false, None);
    assert_eq!(check.status, CheckStatus::Fail);
    // Executable but `--version` yields nothing (or fails): not a pass.
    let check = doctor::decide_tap_binary(true, None);
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.detail.contains("--version"));
}

/// The tap's own pin table: the `PINS` block of `tap/src/main.ts`, the
/// exact source of what `kb-tap --pins` prints, as a JSON object map.
fn tap_reported_pins() -> serde_json::Map<String, serde_json::Value> {
    let main_ts = include_str!("../tap/src/main.ts");
    let start = main_ts
        .find("export const PINS: Record<string, string> = {")
        .expect("tap PINS block");
    let block = &main_ts[start..];
    let block = &block[..block.find("};").expect("end of the PINS block")];
    let mut map = serde_json::Map::new();
    for line in block.lines().skip(1) {
        let quoted: Vec<&str> = line.split('\'').collect();
        // `  '@xterm/headless': '6.0.0',` splits into ["  ", name, ": ", version, ","].
        if quoted.len() >= 4 {
            map.insert(quoted[1].to_string(), serde_json::json!(quoted[3]));
        }
    }
    map
}

#[test]
fn doctor_xterm_pins_pass_on_the_taps_real_report() {
    let reported = tap_reported_pins();
    assert_eq!(
        reported.len(),
        4,
        "the tap pins four packages: {reported:?}"
    );
    // The report is what tap/package.json depends on.
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../tap/package.json")).unwrap();
    assert_eq!(
        manifest["dependencies"],
        serde_json::Value::Object(reported.clone()),
        "tap/src/main.ts PINS drifted from tap/package.json dependencies"
    );
    let json = serde_json::Value::Object(reported).to_string();
    let check = doctor::decide_xterm_pins(Some(&json));
    assert_eq!(check.status, CheckStatus::Pass, "{}", check.detail);
}

#[test]
fn doctor_xterm_pins_fail_on_a_drifted_or_missing_tap_pin() {
    let mut drifted = tap_reported_pins();
    drifted.insert("@xterm/headless".to_string(), serde_json::json!("7.0.0"));
    let check = doctor::decide_xterm_pins(Some(&serde_json::Value::Object(drifted).to_string()));
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.detail.contains("@xterm/headless 7.0.0 != 6.0.0"),
        "{}",
        check.detail
    );
    let mut missing = tap_reported_pins();
    missing.remove("@xterm/addon-serialize");
    let check = doctor::decide_xterm_pins(Some(&serde_json::Value::Object(missing).to_string()));
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.detail.contains("@xterm/addon-serialize missing"),
        "{}",
        check.detail
    );
    // The SPA's viewer-side pins are not the tap's to report: a report that
    // is ONLY those is drift, not a pass.
    let spa_only =
        r#"{"@xterm/xterm":"6.0.0","@xterm/addon-image":"0.9.0","@xterm/addon-webgl":"0.19.0"}"#;
    assert_eq!(
        doctor::decide_xterm_pins(Some(spa_only)).status,
        CheckStatus::Fail
    );
}

#[test]
fn doctor_xterm_pins_skip_when_the_tap_reports_nothing_usable() {
    assert_eq!(doctor::decide_xterm_pins(None).status, CheckStatus::Skip);
    assert_eq!(
        doctor::decide_xterm_pins(Some("not json")).status,
        CheckStatus::Skip
    );
}

#[test]
fn doctor_harness() {
    let check = doctor::decide_harness("omp", Some("18.17.0"), Some("18.17.0"), false);
    assert_eq!(check.status, CheckStatus::Pass);
    assert!(!check.detail.contains("experimental"));
    let check = doctor::decide_harness("omp", Some("18.16.0"), Some("18.17.0"), false);
    assert_eq!(check.status, CheckStatus::Warn);
    // grok never warns on version (unreconciled).
    let check = doctor::decide_harness("grok", Some("0.42.0"), None, false);
    assert_eq!(check.status, CheckStatus::Pass);
    assert!(check.detail.contains("unreconciled"));
    let check = doctor::decide_harness("grok", Some("2.5.1"), None, false);
    assert_eq!(check.status, CheckStatus::Pass);
    let check = doctor::decide_harness("grok", Some("1.0.77"), None, true);
    assert_eq!(check.status, CheckStatus::Pass);
    assert!(check.detail.contains("experimental"));
    let check = doctor::decide_harness("omp", None, Some("18.17.0"), false);
    assert_eq!(check.status, CheckStatus::Skip);
    assert_eq!(check.id, "harness_omp");
    // Exact version-token comparison, not substring: lookalikes warn,
    // a `binary version` line passes.
    let check = doctor::decide_harness("omp", Some("18.17.0.1"), Some("18.17.0"), false);
    assert_eq!(check.status, CheckStatus::Warn);
    let check = doctor::decide_harness("omp", Some("18.17.0-extra"), Some("18.17.0"), false);
    assert_eq!(check.status, CheckStatus::Warn);
    let check = doctor::decide_harness("omp", Some("omp 18.17.0"), Some("18.17.0"), false);
    assert_eq!(check.status, CheckStatus::Pass);
}

#[test]
fn doctor_omp_overlay() {
    let good = "tui:\n  maxInlineImages: 0\n  resizeScrollback: rebuild\ndisplay:\n  collapseCompacted: false\n";
    let check = doctor::decide_omp_overlay(Some(good));
    assert_eq!(check.status, CheckStatus::Pass);
    let bad = "tui:\n  maxInlineImages: 8\n  resizeScrollback: rebuild\ndisplay:\n  collapseCompacted: false\n";
    let check = doctor::decide_omp_overlay(Some(bad));
    assert_eq!(check.status, CheckStatus::Fail);
    // Keys under the wrong top-level section do not satisfy the pins.
    let wrong_section =
        "tui:\n  collapseCompacted: false\ndisplay:\n  maxInlineImages: 0\n  resizeScrollback: rebuild\n";
    let check = doctor::decide_omp_overlay(Some(wrong_section));
    assert_eq!(check.status, CheckStatus::Fail);
    let missing_display = "tui:\n  maxInlineImages: 0\n  resizeScrollback: rebuild\n";
    let check = doctor::decide_omp_overlay(Some(missing_display));
    assert_eq!(check.status, CheckStatus::Fail);
    let check = doctor::decide_omp_overlay(None);
    assert_eq!(check.status, CheckStatus::Skip);
    // The shipped overlay parses clean.
    let check = doctor::decide_omp_overlay(Some(kb_term::profiles::OMP_OVERLAY));
    assert_eq!(check.status, CheckStatus::Pass);
}

fn clean_layout() -> RunLayoutProbe {
    RunLayoutProbe {
        root: Some(DirState {
            mode: 0o755,
            uid: 1000,
        }),
        sessions: Some(DirState {
            mode: 0o700,
            uid: 1000,
        }),
        runner: Some(DirState {
            mode: 0o750,
            uid: 1000,
        }),
        sock: Some(DirState {
            mode: 0o600,
            uid: 1000,
        }),
        cur_uid: 1000,
    }
}

#[test]
fn doctor_run_layout() {
    let check = doctor::decide_run_layout(&clean_layout());
    assert_eq!(check.status, CheckStatus::Pass);
    // A wrong mode fails (0777 example).
    let mut probe = clean_layout();
    probe.sessions = Some(DirState {
        mode: 0o777,
        uid: 1000,
    });
    let check = doctor::decide_run_layout(&probe);
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.fix.is_some());
    // Missing dir fails with the tmpfiles hint.
    let mut probe = clean_layout();
    probe.root = None;
    let check = doctor::decide_run_layout(&probe);
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(check.fix.unwrap().contains("systemd-tmpfiles"));
    // A wrong socket mode fails.
    let mut probe = clean_layout();
    probe.sock = Some(DirState {
        mode: 0o644,
        uid: 1000,
    });
    let check = doctor::decide_run_layout(&probe);
    assert_eq!(check.status, CheckStatus::Fail);
    // Wrong owner fails.
    let mut probe = clean_layout();
    probe.runner = Some(DirState {
        mode: 0o750,
        uid: 999,
    });
    let check = doctor::decide_run_layout(&probe);
    assert_eq!(check.status, CheckStatus::Fail);
    // runner at 0700 also passes.
    let mut probe = clean_layout();
    probe.runner = Some(DirState {
        mode: 0o700,
        uid: 1000,
    });
    let check = doctor::decide_run_layout(&probe);
    assert_eq!(check.status, CheckStatus::Pass);
}

/// The probe reads the socket at the real production path
/// (`<run-dir>/runner/runner.sock`), so a world-readable socket file
/// there FAILS instead of being invisible to a stale path probe.
#[test]
fn doctor_run_layout_probes_the_real_socket() {
    assert_eq!(
        doctor::runner_socket_path(std::path::Path::new("/run/kb-term")),
        std::path::PathBuf::from("/run/kb-term/runner/runner.sock")
    );
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("s")).unwrap();
    std::fs::create_dir_all(root.join("runner")).unwrap();
    for (path, mode) in [
        (root.to_path_buf(), 0o755),
        (root.join("s"), 0o700),
        (root.join("runner"), 0o750),
    ] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let sock = root.join("runner").join("runner.sock");
    std::fs::write(&sock, b"").unwrap();
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o644)).unwrap();
    let probe = doctor::probe_run_layout_at(root);
    let check = doctor::decide_run_layout(&probe);
    assert_eq!(
        check.status,
        CheckStatus::Fail,
        "0644 sock at the real path fails"
    );
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)).unwrap();
    let probe = doctor::probe_run_layout_at(root);
    assert_eq!(doctor::decide_run_layout(&probe).status, CheckStatus::Pass);
}

#[test]
fn doctor_login_env() {
    // Clean: nothing forbidden, POSIX shell.
    let check = doctor::decide_login_env(Some("/bin/bash"), &[], true);
    assert_eq!(check.status, CheckStatus::Pass);
    // Forbidden names warn (names only, never values).
    let forbidden = vec!["KITTY_WINDOW_ID".to_string(), "GHOSTTY_FOO".to_string()];
    let check = doctor::decide_login_env(Some("/bin/bash"), &forbidden, true);
    assert_eq!(check.status, CheckStatus::Warn);
    assert!(check.detail.contains("KITTY_WINDOW_ID"));
    // Non-POSIX shell warns.
    let check = doctor::decide_login_env(Some("/usr/bin/fish"), &[], false);
    assert_eq!(check.status, CheckStatus::Warn);
    // Unset shell skips.
    let check = doctor::decide_login_env(None, &[], false);
    assert_eq!(check.status, CheckStatus::Skip);
    // A failed probe (`None`) is SKIP, never a false PASS.
    let check = doctor::login_env_check("/bin/bash", None);
    assert_eq!(check.status, CheckStatus::Skip);
    // A clean probe passes.
    let check = doctor::login_env_check("/bin/bash", Some(&[]));
    assert_eq!(check.status, CheckStatus::Pass);
    // A probe showing a forbidden name warns (names only).
    let names = vec!["HOME".to_string(), "GHOSTTY_FOO".to_string()];
    let check = doctor::login_env_check("/bin/bash", Some(&names));
    assert_eq!(check.status, CheckStatus::Warn);
}

#[test]
fn doctor_json_never_contains_env_values() {
    // The probe child receives an allowlisted env value (here an
    // allowlisted `XDG_CACHE_HOME` sentinel) and ECHOES it on output
    // line 2; only line 1 is the version line, so neither the JSON nor
    // the human render may carry the value.
    let secret = "fake-secret-value-12345";
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_exe(
        dir.path(),
        "tmux",
        "#!/bin/sh\necho 'tmux 3.7c'\necho \"$XDG_CACHE_HOME\"\n",
    );
    let prev = std::env::var("XDG_CACHE_HOME").ok();
    std::env::set_var("XDG_CACHE_HOME", secret);
    let out = doctor::probe_tmux_version(&fake).unwrap();
    match prev {
        Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
        None => std::env::remove_var("XDG_CACHE_HOME"),
    }
    assert_eq!(out, "tmux 3.7c", "only the version line is kept");
    let checks = vec![doctor::decide_tmux_version(Some(&out))];
    let json = doctor::to_json(&checks);
    let human = doctor::render_human(&checks);
    assert!(!json.contains(secret), "secret leaked into JSON: {json}");
    assert!(
        !human.contains(secret),
        "secret leaked into human output: {human}"
    );
    assert!(json.contains("tmux_version"));
    assert!(json.contains("pass"));
}

/// A hanging binary is killed near the probe deadline instead of
/// blocking doctor: `run_with_timeout` and the probes return `None`
/// after the (3 s / 5 s) timeout, well under the test deadline.
#[test]
fn doctor_probe_timeout_kills_hanging_binary() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_exe(dir.path(), "sleeper", "#!/bin/sh\nexec sleep 30\n");
    let start = std::time::Instant::now();
    assert!(
        doctor::probe_tap_version(&fake).is_none(),
        "a hanging tap must yield None"
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(10),
        "tap probe must kill near its 3 s deadline, took {elapsed:?}"
    );
    let start = std::time::Instant::now();
    let got = doctor::run_with_timeout(
        &fake,
        &["--version"],
        &[],
        std::time::Duration::from_secs(2),
    );
    assert!(got.is_none(), "a hanging binary must yield None");
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "run_with_timeout must kill near its own deadline, took {elapsed:?}"
    );
}
/// A child emitting >64 KiB does not block on a full pipe: output is
/// drained incrementally and each stream is capped at 64 KiB, so the
/// probe returns promptly with a truncated (not full) capture.
#[test]
fn doctor_probe_caps_output_and_never_blocks_on_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_exe(
        dir.path(),
        "loud",
        "#!/bin/sh\nhead -c 200000 /dev/zero | tr '\\000' 'x'\n",
    );
    let start = std::time::Instant::now();
    let got = doctor::run_with_timeout(&fake, &[], &[], std::time::Duration::from_secs(10))
        .expect("loud binary exits");
    assert!(got.0, "loud binary exits zero");
    assert!(
        got.1.len() <= 64 * 1024,
        "stdout capped at 64 KiB, got {} bytes",
        got.1.len()
    );
    assert_eq!(got.1.len(), 64 * 1024, "loud output truncated at cap");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(10),
        "must not block on a full pipe"
    );
}

/// A grandchild holding stdout past the deadline must not hold the
/// probe past the deadline: the process group is killed, not just
/// the direct child, and the grandchild is really gone afterwards.
#[test]
fn doctor_probe_timeout_kills_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("gc.pid");
    let fake = fake_exe(
        dir.path(),
        "forker",
        &format!(
            "#!/bin/sh\nsh -c 'echo $$ > {}; exec sleep 30' &\nexec sleep 30\n",
            pid_file.display()
        ),
    );
    let start = std::time::Instant::now();
    let got = doctor::run_with_timeout(&fake, &[], &[], std::time::Duration::from_secs(2));
    assert!(got.is_none(), "forked sleeper must yield None at deadline");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(8),
        "process-group kill must return near the deadline, took {:?}",
        start.elapsed()
    );
    let gc: u32 = std::fs::read_to_string(&pid_file)
        .expect("grandchild wrote its pid")
        .trim()
        .parse()
        .unwrap();
    let gone_by = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut gone = false;
    while std::time::Instant::now() < gone_by {
        gone = match std::fs::read_to_string(format!("/proc/{gc}/stat")) {
            Ok(st) => {
                st.rsplit(')')
                    .next()
                    .and_then(|s| s.split_whitespace().next())
                    == Some("Z")
            }
            Err(_) => true,
        };
        if gone {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(gone, "the grandchild must be killed with the group");
}

/// The child EXITS normally but a background grandchild still holds
/// stdout: the call must still return by the deadline (not block until
/// the grandchild exits, 30 s here).
#[test]
fn doctor_probe_exit_with_grandchild_holding_stdout_returns_by_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_exe(
        dir.path(),
        "bgjob",
        "#!/bin/sh\nsh -c 'sleep 30' &\necho ok\n",
    );
    let start = std::time::Instant::now();
    let _ = doctor::run_with_timeout(&fake, &[], &[], std::time::Duration::from_secs(2));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(8),
        "a grandchild holding stdout must not block past the deadline, took {:?}",
        start.elapsed()
    );
}

#[test]
fn doctor_exit_codes() {
    let warn = doctor::Check::warn("a", "w");
    let fail = doctor::Check::fail("b", "f");
    let pass = doctor::Check::pass("c", "p");
    let skip = doctor::Check::skip("d", "s");
    // WARN only: 0 by default, 1 under --strict.
    assert_eq!(doctor::exit_code(std::slice::from_ref(&warn), false), 0);
    assert_eq!(doctor::exit_code(std::slice::from_ref(&warn), true), 1);
    // FAIL: 1 always.
    assert_eq!(doctor::exit_code(std::slice::from_ref(&fail), false), 1);
    assert_eq!(doctor::exit_code(std::slice::from_ref(&fail), true), 1);
    // SKIP never fails, even strict.
    assert_eq!(doctor::exit_code(&[skip, pass], false), 0);
    assert_eq!(
        doctor::exit_code(
            &[doctor::Check::skip("d", "s"), doctor::Check::pass("c", "p")],
            true
        ),
        0
    );
}

#[test]
fn doctor_termd_enabled() {
    for ok in ["enabled", "enabled-runtime", "alias", "linked"] {
        assert_eq!(
            doctor::decide_termd_enabled(Some(ok)).status,
            CheckStatus::Pass,
            "{ok}"
        );
    }
    for bad in ["static", "disabled", "masked"] {
        let c = doctor::decide_termd_enabled(Some(bad));
        assert_eq!(c.status, CheckStatus::Warn, "{bad}");
        assert_eq!(c.id, "termd_enabled");
        assert!(c.detail.contains(bad));
    }
    assert_eq!(doctor::decide_termd_enabled(None).status, CheckStatus::Skip);
}

#[test]
fn doctor_parse_environ_splits_nul_pairs() {
    let env = doctor::parse_environ(b"PATH=/usr/bin:/bin\0KB_SESSIONS_DIR=\0X=a=b\0junk\0");
    assert_eq!(env.len(), 3);
    assert_eq!(env[0], ("PATH".to_string(), "/usr/bin:/bin".to_string()));
    assert_eq!(env[1].1, "");
    assert_eq!(env[2], ("X".to_string(), "a=b".to_string()));
}

fn env_of(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn status_of(checks: &[doctor::Check], id: &str) -> CheckStatus {
    checks.iter().find(|c| c.id == id).unwrap().status
}

#[test]
fn doctor_runner_env_resolves_against_the_runner_not_the_shell() {
    let dir = tempfile::tempdir().unwrap();
    let runner_bin = dir.path().join("runner-bin");
    let shell_bin = dir.path().join("shell-bin");
    std::fs::create_dir_all(&runner_bin).unwrap();
    std::fs::create_dir_all(&shell_bin).unwrap();
    fake_exe(&runner_bin, "kb", "#!/bin/sh\n");
    fake_exe(&runner_bin, "claude", "#!/bin/sh\n");
    let omp = fake_exe(&shell_bin, "omp", "#!/bin/sh\n");
    let env = env_of(&[
        ("PATH", runner_bin.to_str().unwrap()),
        ("KB_SESSIONS_DIR", "/srv/sessions"),
    ]);
    let harnesses = vec![
        ("claude".to_string(), Some(runner_bin.join("claude"))),
        ("omp".to_string(), Some(omp)),
        ("grok".to_string(), None),
    ];
    let checks = doctor::decide_runner_env(Some(&env), &harnesses);
    assert_eq!(
        status_of(&checks, "runner_env_sessions_dir"),
        CheckStatus::Pass
    );
    assert_eq!(status_of(&checks, "runner_env_kb"), CheckStatus::Pass);
    assert_eq!(
        status_of(&checks, "runner_env_harness_claude"),
        CheckStatus::Pass
    );
    // omp is on the shell PATH only: launching it would exit 127, but the
    // runner works, so this is a WARN (never a FAIL that exits 1 for every
    // installed-but-unused harness).
    assert_eq!(
        status_of(&checks, "runner_env_harness_omp"),
        CheckStatus::Warn
    );
    assert!(
        checks.iter().all(|c| c.status != CheckStatus::Fail),
        "an off-PATH harness alone must not fail the doctor"
    );
    // Not installed anywhere: no check at all.
    assert!(checks.iter().all(|c| c.id != "runner_env_harness_grok"));
}

#[test]
fn doctor_runner_env_fails_when_sessions_dir_is_unset_or_empty() {
    for env in [
        env_of(&[("PATH", "/usr/bin")]),
        env_of(&[("PATH", "/usr/bin"), ("KB_SESSIONS_DIR", "")]),
    ] {
        let checks = doctor::decide_runner_env(Some(&env), &[]);
        let c = checks
            .iter()
            .find(|c| c.id == "runner_env_sessions_dir")
            .unwrap();
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.fix.as_deref().unwrap_or("").contains("env.conf"));
        assert_eq!(status_of(&checks, "runner_env_kb"), CheckStatus::Warn);
    }
}

#[test]
fn doctor_runner_env_skips_when_the_runner_is_unreadable() {
    let checks = doctor::decide_runner_env(None, &[("omp".to_string(), None)]);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, CheckStatus::Skip);
}

// ---- FXTERMCLI: honest harness probe outcomes -------------------------

fn probe(dir: &std::path::Path, body: &str, secs: u64) -> doctor::HarnessProbe {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let exe = fake_exe(dir, &format!("harness{n}"), body);
    doctor::probe_harness_at(&exe, &[], std::time::Duration::from_secs(secs))
}

#[test]
fn doctor_harness_probe_tells_failure_timeout_and_silence_apart() {
    let dir = tempfile::tempdir().unwrap();
    // A good version line.
    match probe(dir.path(), "#!/bin/sh\necho 'tool 2.2.7'\n", 5) {
        doctor::HarnessProbe::Version { line, .. } => assert_eq!(line, "tool 2.2.7"),
        other => panic!("{other:?}"),
    }
    // A non-zero exit carries the code and the first stderr line.
    let failed = probe(dir.path(), "#!/bin/sh\necho 'no HOME set' >&2\nexit 7\n", 5);
    assert!(matches!(
        &failed,
        doctor::HarnessProbe::Failed { exit: Some(7), stderr, .. } if stderr == "no HOME set"
    ));
    // A hang is a timeout, not a failure and not "not found".
    let slow = probe(dir.path(), "#!/bin/sh\nexec sleep 30\n", 1);
    assert!(matches!(
        &slow,
        doctor::HarnessProbe::TimedOut { secs: 1, .. }
    ));
    // Exit 0 with no output is its own case.
    let silent = probe(dir.path(), "#!/bin/sh\nexit 0\n", 5);
    assert!(matches!(&silent, doctor::HarnessProbe::NoOutput { .. }));
    // Verdicts: the three found-but-unverified cases WARN naming the
    // path and the cause, and none claims the binary is missing.
    for (p, want) in [
        (&failed, "exit 7"),
        (&slow, "timed out after 1s"),
        (&silent, "printed no version"),
    ] {
        let c = doctor::decide_harness_probe("opencode", p, None, false);
        assert_eq!(c.status, CheckStatus::Warn, "{p:?}");
        assert!(c.detail.contains(want), "{}", c.detail);
        assert!(c.detail.contains("found at"), "{}", c.detail);
        assert!(!c.detail.contains("not found"), "{}", c.detail);
    }
    // Only a genuinely absent binary is "not found" (SKIP).
    let c = doctor::decide_harness_probe("opencode", &doctor::HarnessProbe::NotFound, None, false);
    assert_eq!(c.status, CheckStatus::Skip);
    assert!(c.detail.contains("not found"));
    // A spawn failure (not executable content) is reported as such.
    let exe = dir.path().join("noexec");
    std::fs::write(&exe, "x").unwrap();
    let c = doctor::probe_harness_at(&exe, &[], std::time::Duration::from_secs(2));
    assert!(matches!(c, doctor::HarnessProbe::NoOutput { .. }), "{c:?}");
}

// ---- FXTERMCLI: injected host seams ------------------------------------

#[derive(Default)]
struct FakeHost {
    /// `show MainPID` answer: None = systemctl unavailable.
    main_pid: Option<(bool, String)>,
    enabled: Option<String>,
    procs: Vec<(u32, String, Option<u32>)>,
    uid: Option<u32>,
    environ: std::collections::HashMap<u32, Vec<u8>>,
    asked: std::cell::RefCell<Vec<String>>,
}

impl doctor::HostProbe for FakeHost {
    fn systemctl_user(&self, args: &[&str]) -> Option<(bool, String, String)> {
        self.asked.borrow_mut().push(args.join(" "));
        match args.first().copied() {
            Some("show") => self
                .main_pid
                .clone()
                .map(|(ok, out)| (ok, out, String::new())),
            Some("is-enabled") => self.enabled.clone().map(|o| (true, o, String::new())),
            _ => None,
        }
    }
    fn processes(&self) -> Vec<(u32, String, Option<u32>)> {
        self.procs.clone()
    }
    fn my_uid(&self) -> Option<u32> {
        self.uid
    }
    fn read_environ(&self, pid: u32) -> Option<Vec<u8>> {
        self.environ.get(&pid).cloned()
    }
}

fn termd(pid: u32, uid: u32) -> (u32, String, Option<u32>) {
    (pid, "kb-termd".to_string(), Some(uid))
}

#[test]
fn doctor_runner_pid_prefers_mainpid_over_a_stray_same_uid_process() {
    // The unit's MainPID wins even when another kb-termd of ours exists.
    let host = FakeHost {
        main_pid: Some((true, "4242\n".into())),
        procs: vec![termd(1111, 1000)],
        uid: Some(1000),
        ..FakeHost::default()
    };
    assert_eq!(doctor::probe_runner_pid_with(&host), Some(4242));
    // MainPID=0 (unit stopped): systemd answered, so a stray process is NOT
    // adopted as the runner.
    let host = FakeHost {
        main_pid: Some((true, "0\n".into())),
        procs: vec![termd(1111, 1000)],
        uid: Some(1000),
        ..FakeHost::default()
    };
    assert_eq!(doctor::probe_runner_pid_with(&host), None);
}

#[test]
fn doctor_runner_pid_proc_fallback_only_when_systemd_cannot_answer() {
    let procs = vec![
        (1, "init".to_string(), Some(0)),
        termd(900, 2000), // another user's runner
        termd(901, 1000),
        (902, "kb-termd-x".to_string(), Some(1000)),
    ];
    for unavailable in [
        None,
        Some((false, String::new())),
        Some((true, "junk\n".into())),
    ] {
        let host = FakeHost {
            main_pid: unavailable,
            procs: procs.clone(),
            uid: Some(1000),
            ..FakeHost::default()
        };
        assert_eq!(doctor::probe_runner_pid_with(&host), Some(901));
    }
    // Two of our own: ambiguous, so none.
    let mut two = procs.clone();
    two.push(termd(903, 1000));
    let host = FakeHost {
        procs: two,
        uid: Some(1000),
        ..FakeHost::default()
    };
    assert_eq!(doctor::probe_runner_pid_with(&host), None);
    // Unknown own uid: nothing is matched.
    let host = FakeHost {
        procs,
        uid: None,
        ..FakeHost::default()
    };
    assert_eq!(doctor::probe_runner_pid_with(&host), None);
}

#[test]
fn doctor_termd_enabled_reads_the_state_word_through_the_seam() {
    let host = FakeHost {
        enabled: Some("static\n".into()),
        ..FakeHost::default()
    };
    assert_eq!(
        doctor::probe_termd_enabled_with(&host).as_deref(),
        Some("static")
    );
    assert_eq!(
        doctor::probe_termd_enabled_with(&FakeHost::default()),
        None,
        "systemd unavailable is None, which decides to SKIP"
    );
}

/// The wiring `run_checks` uses: boot enablement, then the runner env read
/// from the pid the unit names, through the seam only.
#[test]
fn doctor_host_state_checks_wire_pid_environ_and_enablement_together() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fake_exe(&bin, "kb", "#!/bin/sh\n");
    let mut environ = std::collections::HashMap::new();
    environ.insert(
        4242,
        format!("PATH={}\0KB_SESSIONS_DIR=/s\0", bin.display()).into_bytes(),
    );
    // A different kb-termd with a bad env must not be the one read.
    environ.insert(1111, b"PATH=/nowhere\0".to_vec());
    let host = FakeHost {
        main_pid: Some((true, "4242\n".into())),
        enabled: Some("enabled\n".into()),
        procs: vec![termd(1111, 1000)],
        uid: Some(1000),
        environ,
        ..FakeHost::default()
    };
    let checks = doctor::host_state_checks(&host, &[]);
    assert_eq!(status_of(&checks, "termd_enabled"), CheckStatus::Pass);
    assert_eq!(
        status_of(&checks, "runner_env_sessions_dir"),
        CheckStatus::Pass
    );
    assert_eq!(status_of(&checks, "runner_env_kb"), CheckStatus::Pass);
    // Unreadable environ: one runner_env SKIP, still alongside termd_enabled.
    let host = FakeHost {
        main_pid: Some((true, "4242\n".into())),
        enabled: Some("disabled\n".into()),
        uid: Some(1000),
        ..FakeHost::default()
    };
    let checks = doctor::host_state_checks(&host, &[]);
    assert_eq!(status_of(&checks, "termd_enabled"), CheckStatus::Warn);
    assert_eq!(status_of(&checks, "runner_env"), CheckStatus::Skip);
}
