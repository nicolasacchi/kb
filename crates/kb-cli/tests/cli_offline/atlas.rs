//! `kb atlas recompute` smoke. Doesn't spin up a real daemon — these
//! are pure CLI invocations against `--daemon http://127.0.0.1:0`
//! (a port that won't connect), so the test asserts the failure mode
//! is surfaced cleanly (clap parses + handler emits a connect error).
//! The success path is exercised in the kb-server end_to_end suite +
//! the X7 manual smoke.

use assert_cmd::Command;

#[test]
fn atlas_recompute_help_lists_kb_flag() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["atlas", "recompute", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--kb"), "missing --kb in help: {text}");
    assert!(
        text.contains("--daemon"),
        "missing --daemon in help: {text}"
    );
}

#[test]
fn atlas_recompute_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "atlas",
            "recompute",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1", // EOF/connect refused
        ])
        .assert()
        .failure();
}

// --- W3 T-b: history / show / prune ------------------------------------
//
// Same posture as recompute above: no real daemon, just asserting clap
// wiring + a clean failure mode. Success paths are exercised in the
// kb-server end_to_end suite's atlas history/show/prune tests.

#[test]
fn atlas_history_help_lists_kb_flag() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["atlas", "history", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--kb"), "missing --kb in help: {text}");
    assert!(text.contains("--json"), "missing --json in help: {text}");
}

#[test]
fn atlas_history_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "atlas",
            "history",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn atlas_show_help_lists_id_and_align_to() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["atlas", "show", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--kb"), "missing --kb in help: {text}");
    assert!(
        text.contains("--align-to"),
        "missing --align-to in help: {text}"
    );
}

#[test]
fn atlas_show_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "atlas",
            "show",
            "1",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn atlas_prune_help_lists_keep_flag() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["atlas", "prune", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--kb"), "missing --kb in help: {text}");
    assert!(text.contains("--keep"), "missing --keep in help: {text}");
}

#[test]
fn atlas_prune_requires_keep() {
    // `--keep` is a required flag (no implicit default — see the route's
    // doc comment) — clap should reject its absence before ever reaching
    // the network.
    Command::cargo_bin("kb")
        .unwrap()
        .args(["atlas", "prune", "--kb", "smoke"])
        .assert()
        .failure();
}

#[test]
fn atlas_prune_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "atlas",
            "prune",
            "--kb",
            "smoke",
            "--keep",
            "5",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}
