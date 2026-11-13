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

#[test]
fn doctor_xterm_pins() {
    let ok_json = r#"{"@xterm/xterm":"6.0.0","@xterm/headless":"6.0.0","@xterm/addon-serialize":"0.14.0","@xterm/addon-unicode-graphemes":"0.4.0","@xterm/addon-image":"0.9.0","@xterm/addon-webgl":"0.19.0"}"#;
    let check = doctor::decide_xterm_pins(Some(ok_json));
    assert_eq!(check.status, CheckStatus::Pass);
    let drift_json = r#"{"@xterm/xterm":"7.0.0"}"#;
    let check = doctor::decide_xterm_pins(Some(drift_json));
    assert_eq!(check.status, CheckStatus::Fail);
    let check = doctor::decide_xterm_pins(None);
    assert_eq!(check.status, CheckStatus::Skip);
    let check = doctor::decide_xterm_pins(Some("not json"));
    assert_eq!(check.status, CheckStatus::Skip);
}

#[test]
fn doctor_harness() {
    let check = doctor::decide_harness("omp", Some("18.17.0"), Some("18.17.0"), false);
    assert_eq!(check.status, CheckStatus::Pass);
    assert!(!check.detail.contains("experimental"));
    let check = doctor::decide_harness("omp", Some("18.16.0"), Some("18.17.0"), false);
    assert_eq!(check.status, CheckStatus::Warn);
    // kimi and grok never warn on version (unreconciled).
    let check = doctor::decide_harness("kimi", Some("0.42.0"), None, false);
    assert_eq!(check.status, CheckStatus::Pass);
    assert!(check.detail.contains("unreconciled"));
    let check = doctor::decide_harness("kimi", Some("2.5.1"), None, false);
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
