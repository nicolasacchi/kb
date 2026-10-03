//! `kb chores --line` is called from a SessionStart hook (v0.44 F10): with
//! the daemon down it must exit 0 and print NOTHING (no error into the
//! session), while the plain verb still reports the failure.

use assert_cmd::Command;

/// A port nothing listens on; `detect_daemon` fails fast (connection refused).
const DEAD: &str = "http://127.0.0.1:1";

#[test]
fn line_is_silent_and_exit_zero_when_the_daemon_is_down() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["chores", "--line", "--daemon", DEAD])
        .assert()
        .success()
        .stdout("")
        .stderr("");
}

#[test]
fn plain_chores_still_reports_an_unreachable_daemon() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["chores", "--daemon", DEAD])
        .assert()
        .failure();
}
