//! R1 — `kb reindex` CLI smoke. Pure CLI invocations against an
//! unreachable URL; the daemon-attached path is covered by the
//! kb-server e2e tests.

use assert_cmd::Command;

#[test]
fn reindex_help_lists_kb_daemon_json() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["reindex", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--kb"), "missing --kb");
    assert!(text.contains("--daemon"), "missing --daemon");
    assert!(text.contains("--json"), "missing --json");
}

#[test]
fn reindex_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["reindex", "--kb", "smoke", "--daemon", "http://127.0.0.1:1"])
        .assert()
        .failure();
}
