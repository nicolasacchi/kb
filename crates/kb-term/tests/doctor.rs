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
    assert!(check.detail.contains("experimental") == false);
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
}

#[test]
fn doctor_omp_overlay() {
    let good = "tui:\n  maxInlineImages: 0\n  resizeScrollback: rebuild\ndisplay:\n  collapseCompacted: false\n";
    let check = doctor::decide_omp_overlay(Some(good));
    assert_eq!(check.status, CheckStatus::Pass);
    let bad = "tui:\n  maxInlineImages: 8\n  resizeScrollback: rebuild\ndisplay:\n  collapseCompacted: false\n";
    let check = doctor::decide_omp_overlay(Some(bad));
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
}

#[test]
fn doctor_json_never_contains_env_values() {
    // A fake secret in the probe env must never appear in JSON output.
    let secret = "fake-secret-value-12345";
    let dir = tempfile::tempdir().unwrap();
    let fake = fake_exe(
        dir.path(),
        "tmux",
        &format!("#!/bin/sh\necho 'tmux 3.7c' # {secret}\n"),
    );
    let out = doctor::probe_tmux_version(&fake).unwrap();
    let checks = vec![doctor::decide_tmux_version(Some(&out))];
    let json = doctor::to_json(&checks);
    assert!(!json.contains(secret), "secret leaked into JSON: {json}");
    assert!(json.contains("tmux_version"));
    assert!(json.contains("pass"));
}

#[test]
fn doctor_exit_codes() {
    let warn = doctor::Check::warn("a", "w");
    let fail = doctor::Check::fail("b", "f");
    let pass = doctor::Check::pass("c", "p");
    let skip = doctor::Check::skip("d", "s");
    // WARN only: 0 by default, 1 under --strict.
    assert_eq!(doctor::exit_code(&[warn.clone()], false), 0);
    assert_eq!(doctor::exit_code(&[warn.clone()], true), 1);
    // FAIL: 1 always.
    assert_eq!(doctor::exit_code(&[fail.clone()], false), 1);
    assert_eq!(doctor::exit_code(&[fail.clone()], true), 1);
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
