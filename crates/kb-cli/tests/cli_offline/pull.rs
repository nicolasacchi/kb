//! `kb pull` CLI smoke. Follows the fleet/push convention: no real
//! daemon is booted — these assert the help surface and the OIDC
//! argument-validation error paths, which fail before any network call.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn pull_help_lists_flags() {
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args(["pull", "--help"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    for flag in [
        "--from",
        "--kb",
        "--into",
        "--oidc-token-url",
        "--oidc-client-id",
        "--scope",
    ] {
        assert!(out.contains(flag), "missing {flag} in help, got: {out}");
    }
}

#[test]
fn pull_rejects_lone_oidc_flag() {
    // Only --oidc-token-url (no client id) → usage error, before any I/O.
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "pull",
            "--from",
            "http://127.0.0.1:55556",
            "--kb",
            "smoke",
            "--into",
            "/tmp/kb-pull-test-unused",
            "--oidc-token-url",
            "http://127.0.0.1:55556/token",
        ])
        .assert()
        .failure()
        .stderr(contains("must be supplied together"));
}

#[test]
fn pull_errors_when_oidc_secret_env_missing() {
    // Both OIDC flags present but no KB_OIDC_CLIENT_SECRET → clear error.
    Command::cargo_bin("kb")
        .unwrap()
        .env_remove("KB_OIDC_CLIENT_SECRET")
        .args([
            "pull",
            "--from",
            "http://127.0.0.1:55556",
            "--kb",
            "smoke",
            "--into",
            "/tmp/kb-pull-test-unused",
            "--oidc-token-url",
            "http://127.0.0.1:55556/token",
            "--oidc-client-id",
            "kb-bot",
        ])
        .assert()
        .failure()
        .stderr(contains("KB_OIDC_CLIENT_SECRET"));
}
