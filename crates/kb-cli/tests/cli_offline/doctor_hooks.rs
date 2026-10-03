//! `kb doctor --hooks` integration tests. Exercises CLI wiring end-to-end
//! (arg parsing, HTTP-check plumbing, JSON shape) against a DEAD daemon
//! endpoint and a plain (non-git) tempdir `--repo` — no live daemon
//! needed, mirroring `doctor.rs`'s (`kb daemon doctor`) own convention.
//! The interesting decision logic is unit-tested directly in
//! `commands::doctor`; this file only proves the verb is wired up.

use assert_cmd::Command;
use predicates::str::contains;

fn dead_endpoint() -> String {
    "http://127.0.0.1:55556".to_string()
}

#[test]
fn doctor_bare_without_hooks_flag_errors() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["doctor"])
        .assert()
        .failure()
        .stderr(contains("--hooks"));
}

#[test]
fn doctor_hooks_human_output_against_a_non_git_repo_and_dead_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "doctor",
            "--hooks",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--daemon",
            &dead_endpoint(),
        ])
        .assert()
        .success()
        .stdout(contains("provenance-chain integrity check"))
        .stdout(contains("git-trailer-hook"))
        .stdout(contains("harness scope"));
}

#[test]
fn doctor_hooks_json_output_is_valid_and_has_expected_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "doctor",
            "--hooks",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--daemon",
            &dead_endpoint(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value =
        serde_json::from_slice(&out).expect("--json should emit valid JSON");
    let checks = parsed["checks"].as_array().expect("checks array");
    assert!(!checks.is_empty());
    // The repo is a plain tempdir (no `git init`) — the git-trailer-hook
    // check must SKIP, never silently PASS.
    let git_check = checks
        .iter()
        .find(|c| c["id"] == "git-trailer-hook")
        .expect("git-trailer-hook check present");
    assert_eq!(git_check["status"], "skip");
    // The daemon is unreachable — the sessions-kb check must WARN, not PASS.
    let daemon_check = checks
        .iter()
        .find(|c| c["id"] == "daemon-sessions-kb")
        .expect("daemon-sessions-kb check present");
    assert_eq!(daemon_check["status"], "warn");
    let notes = parsed["notes"].as_array().expect("notes array");
    assert!(notes
        .iter()
        .any(|n| n.as_str().unwrap().contains("Claude Code only")));
}

/// F8 (A4.f4) — `--strict` turns the report into a gate. Against a dead
/// daemon the `daemon-sessions-kb` check WARNs, so strict exits 1 while the
/// default stays exit 0; the cli-skew check is present either way.
#[test]
fn doctor_hooks_strict_exits_nonzero_on_a_warn_and_default_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let args = |strict: bool| {
        let mut a = vec![
            "doctor".to_string(),
            "--hooks".into(),
            "--repo".into(),
            tmp.path().to_str().unwrap().into(),
            "--daemon".into(),
            dead_endpoint(),
            "--json".into(),
        ];
        if strict {
            a.push("--strict".into());
        }
        a
    };
    Command::cargo_bin("kb")
        .unwrap()
        .args(args(false))
        .assert()
        .success();
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(args(true))
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("--json under --strict");
    let ids: Vec<&str> = parsed["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"cli-skew"), "{ids:?}");
}
