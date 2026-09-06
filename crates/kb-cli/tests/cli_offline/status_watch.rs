//! `kb status --watch` — guard the mutual-exclusion contract with
//! `--json` and verify the flag parses. Running the actual watch loop
//! would loop forever (Ctrl+C is its only exit); the test stays at the
//! arg-parse + early-exit boundary.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn status_watch_with_json_is_rejected() {
    // Same shape as `doctor --watch --json` — scripted callers want
    // one-shot output, the interactive watch loop must not engage.
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_STATE_DIR", tmp.path().join("state"))
        .env("KB_CONFIG_DIR", tmp.path().join("config"))
        .env("KB_CACHE_DIR", tmp.path().join("cache"))
        .args(["status", "--watch", "1", "--json"])
        .assert()
        .failure()
        .stderr(contains("mutually exclusive"));
}

#[test]
fn status_watch_flag_parses() {
    // `--watch` without `--json` enters the loop; we can't run that to
    // completion in a test, so we verify clap accepts the flag by
    // calling `--help` (which short-circuits before any loop runs).
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args(["status", "--help"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        out.contains("--watch"),
        "--watch missing from `kb status --help`"
    );
}
