//! D1 — `--json` audit smoke. Each verb's `--json` output must
//! parse via serde_json. We don't try to validate the schema here
//! (the human-format tests already cover content); this is a
//! "doesn't crash + valid JSON" guarantee.

use assert_cmd::Command;

fn parse_json_stdout(args: &[&str]) -> serde_json::Value {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    serde_json::from_str(&text).unwrap_or_else(|e| {
        panic!(
            "expected JSON output for `kb {}`; got: {text}\nerror: {e}",
            args.join(" ")
        )
    })
}

#[test]
fn status_json_emits_object_with_kbs_array() {
    let tmp = tempfile::tempdir().unwrap();
    let value = {
        let out = Command::cargo_bin("kb")
            .unwrap()
            .env("KB_STATE_DIR", tmp.path().join("state"))
            .env("KB_CONFIG_DIR", tmp.path().join("config"))
            .env("KB_CACHE_DIR", tmp.path().join("cache"))
            // Dead endpoint so this offline test does not pick up a live
            // daemon on :4000 now that status lists /api/stats kbs.
            .env("KB_DAEMON_URL", "http://127.0.0.1:1")
            .args(["status", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8(out).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()
    };
    assert!(value["daemon_name"].is_string());
    assert!(value["kbs"].is_array());
}

// `comments list --json` is now HTTP-only (R6) — its valid-JSON-array
// contract is covered against a live daemon in `comments.rs`
// (`list_and_export_round_trip_against_daemon`), not here.

#[test]
fn search_json_help_lists_flag() {
    // Smoke: --json appears in the help output for kb search.
    let _ = parse_json_stdout; // suppress dead-code if tests above don't use it
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["search", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--json"), "missing --json in help: {text}");
}

#[test]
fn status_json_help_lists_flag() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["status", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--json"), "missing --json in help: {text}");
}
