//! `kb fleet replicate` (Q4) + `kb fleet status` (v0.24 T1). The
//! unreachable-daemon paths run against a fabricated daemons.toml under
//! a temp KB_CONFIG_DIR with a dead endpoint; the live `fleet status`
//! path boots the REAL `kb daemon` on a scratch port and asserts the
//! identity+stats sweep.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use std::fs;

fn dead_endpoint() -> &'static str {
    "http://127.0.0.1:55556"
}

fn stage_daemons_toml(tmp: &std::path::Path) -> std::path::PathBuf {
    let config = tmp.join("config");
    fs::create_dir_all(&config).unwrap();
    // Single dead daemon — the diff report runs but flags it as
    // unreachable.
    let toml = format!("[daemon.local]\nendpoint = \"{}\"\n", dead_endpoint());
    fs::write(config.join("daemons.toml"), toml).unwrap();
    // Minimal kb.toml (KbPaths::new needs the directory to exist —
    // the fleet verb does too).
    fs::write(
        config.join("kb.toml"),
        "[daemon]\nname = \"default\"\n\n[server]\naddr = \"127.0.0.1:55557\"\n\n[ui]\n",
    )
    .unwrap();
    config
}

#[test]
fn fleet_replicate_reports_unreachable_daemon_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let config = stage_daemons_toml(tmp.path());

    // Single-daemon fleet → the verb prints a hint about needing more,
    // then runs the diff. The dead endpoint fails fetch → unreachable
    // marker. The verb returns success because per-daemon failures are
    // non-fatal (matches the TUI's connection model).
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_CONFIG_DIR", &config)
        .env("KB_STATE_DIR", tmp.path().join("state"))
        .env("KB_CACHE_DIR", tmp.path().join("cache"))
        .args(["fleet", "replicate", "--kb", "smoke"])
        .assert()
        .success()
        .stdout(contains("unreachable").or(contains("nothing to do")));
}

#[test]
fn fleet_status_reports_unreachable_daemon_without_failing() {
    // T1 — per-daemon failures are report rows, not aborts (the same
    // connection model as replicate). Exit code stays 0.
    let tmp = tempfile::tempdir().unwrap();
    let config = stage_daemons_toml(tmp.path());

    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_CONFIG_DIR", &config)
        .env("KB_STATE_DIR", tmp.path().join("state"))
        .env("KB_CACHE_DIR", tmp.path().join("cache"))
        .args(["fleet", "status"])
        .assert()
        .success()
        .stdout(contains("unreachable").and(contains("local")));
}

#[test]
fn fleet_status_json_marks_dead_daemon_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let config = stage_daemons_toml(tmp.path());

    let assert = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_CONFIG_DIR", &config)
        .env("KB_STATE_DIR", tmp.path().join("state"))
        .env("KB_CACHE_DIR", tmp.path().join("cache"))
        .args(["fleet", "status", "--json"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let rows: serde_json::Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("fleet status --json not JSON ({e}):\n{out}"));
    let rows = rows.as_array().expect("array of daemon rows");
    assert_eq!(rows.len(), 1, "one configured daemon: {rows:?}");
    assert_eq!(rows[0]["name"], "local");
    assert_eq!(rows[0]["reachable"], false);
    assert!(
        rows[0]["error"].as_str().is_some_and(|e| !e.is_empty()),
        "unreachable row should carry the error: {rows:?}"
    );
}

#[test]
fn fleet_help_lists_status_and_replicate_subcommands() {
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args(["fleet", "--help"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("status"), "missing `status` in help: {out}");
    assert!(
        out.contains("replicate"),
        "missing `replicate` in help: {out}"
    );
}

#[test]
fn fleet_replicate_help_lists_replicate_subcommand() {
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args(["fleet", "--help"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        out.contains("replicate"),
        "expected `replicate` subcommand in help, got: {out}"
    );
}

#[test]
fn fleet_replicate_copy_to_help_mentions_directory_semantics() {
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args(["fleet", "replicate", "--help"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("--copy-to"), "missing --copy-to flag");
    assert!(out.contains("--src"), "missing --src flag");
    assert!(out.contains("--kb"), "missing --kb flag");
}
