//! `kb push` CLI smoke. Doesn't spin up a real daemon — these are
//! pure CLI invocations against a daemon-unreachable URL, asserting
//! the help text + exit-code behavior. The full daemon-attached path
//! is exercised in the X2 manual smoke (the SSE stream loop runs
//! forever, so it needs a SIGTERM-style stop in a real test setup).

use assert_cmd::Command;

#[test]
fn push_help_lists_filter_and_daemon_flags() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["push", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--filter"), "missing --filter in help");
    assert!(text.contains("--daemon"), "missing --daemon in help");
}
