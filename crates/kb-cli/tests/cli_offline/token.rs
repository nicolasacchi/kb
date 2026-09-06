//! `kb token {generate, rotate, show, path}` integration tests.
//! Sandboxed via `KB_HOME` per test so concurrent test runs don't collide
//! over the real token file (and so concurrent Linux tests isolate
//! even when `directories` would share XDG paths).

use assert_cmd::Command;

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap().mode() & 0o777
}

#[test]
fn generate_writes_token_file_with_mode_0600() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args(["token", "generate"])
        .assert()
        .success();
    let token_path = tmp.path().join("config").join("token");
    assert!(token_path.exists(), "token not written");
    let body = std::fs::read_to_string(&token_path).unwrap();
    assert!(body.trim().len() >= 32, "token too short: {body:?}");
    #[cfg(unix)]
    assert_eq!(mode_of(&token_path), 0o600);
}

#[test]
fn generate_refuses_to_overwrite_existing_token() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args(["token", "generate"])
        .assert()
        .success();
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args(["token", "generate"])
        .assert()
        .failure();
}

#[test]
fn rotate_overwrites_existing_token() {
    let tmp = tempfile::tempdir().unwrap();
    let env_args = [("KB_HOME", tmp.path().to_path_buf())];
    Command::cargo_bin("kb")
        .unwrap()
        .envs(env_args.clone())
        .args(["token", "generate"])
        .assert()
        .success();
    let token_path = tmp.path().join("config").join("token");
    let first = std::fs::read_to_string(&token_path).unwrap();

    Command::cargo_bin("kb")
        .unwrap()
        .envs(env_args)
        .args(["token", "rotate"])
        .assert()
        .success();
    let second = std::fs::read_to_string(&token_path).unwrap();
    assert_ne!(first, second, "rotate should produce a different token");
}

#[test]
fn show_print_writes_token_to_stdout() {
    let tmp = tempfile::tempdir().unwrap();
    let env_args = [("KB_HOME", tmp.path().to_path_buf())];
    Command::cargo_bin("kb")
        .unwrap()
        .envs(env_args.clone())
        .args(["token", "generate"])
        .assert()
        .success();
    let out = Command::cargo_bin("kb")
        .unwrap()
        .envs(env_args)
        .args(["token", "show", "--print"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let printed = String::from_utf8(out).unwrap();
    assert!(
        printed.trim().len() >= 32,
        "stdout token too short: {printed:?}"
    );
}

#[test]
fn path_prints_token_file_location() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args(["token", "path"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let printed = String::from_utf8(out).unwrap();
    assert!(printed.trim().ends_with("config/token"), "got: {printed:?}");
}

// --- v0.34 Z1: multi-user registry (issue / revoke) ----------------------

#[test]
fn issue_writes_sha256_registry_line_mode_0600() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args(["token", "issue", "alice"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plaintext = String::from_utf8(out).unwrap();
    let plaintext = plaintext.trim();
    assert!(plaintext.len() >= 32, "plaintext too short: {plaintext:?}");

    let reg = tmp.path().join("config").join("tokens");
    assert!(reg.exists(), "tokens registry not written");
    let body = std::fs::read_to_string(&reg).unwrap();
    assert!(
        body.lines().any(|l| l.starts_with("alice:sha256:")),
        "expected alice:sha256:… line, got: {body:?}"
    );
    // Plaintext must NOT land in the file.
    assert!(
        !body.contains(plaintext),
        "plaintext leaked into registry: {body:?}"
    );
    #[cfg(unix)]
    assert_eq!(mode_of(&reg), 0o600);
}

#[test]
fn issue_refuses_duplicate_without_force() {
    let tmp = tempfile::tempdir().unwrap();
    let env = [("KB_HOME", tmp.path().to_path_buf())];
    Command::cargo_bin("kb")
        .unwrap()
        .envs(env.clone())
        .args(["token", "issue", "alice"])
        .assert()
        .success();
    Command::cargo_bin("kb")
        .unwrap()
        .envs(env)
        .args(["token", "issue", "alice"])
        .assert()
        .failure();
}

#[test]
fn issue_force_replaces_existing_line() {
    let tmp = tempfile::tempdir().unwrap();
    let env = [("KB_HOME", tmp.path().to_path_buf())];
    let first_out = Command::cargo_bin("kb")
        .unwrap()
        .envs(env.clone())
        .args(["token", "issue", "alice"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let first = String::from_utf8(first_out).unwrap();
    let reg = tmp.path().join("config").join("tokens");
    let body1 = std::fs::read_to_string(&reg).unwrap();

    let second_out = Command::cargo_bin("kb")
        .unwrap()
        .envs(env)
        .args(["token", "issue", "alice", "--force"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let second = String::from_utf8(second_out).unwrap();
    assert_ne!(first.trim(), second.trim(), "force should mint a new token");

    let body2 = std::fs::read_to_string(&reg).unwrap();
    assert_ne!(body1, body2);
    // Still exactly one alice line.
    let alice_lines: Vec<_> = body2.lines().filter(|l| l.starts_with("alice:")).collect();
    assert_eq!(alice_lines.len(), 1, "got: {body2:?}");
}

#[test]
fn issue_rejects_invalid_username() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args(["token", "issue", "Alice"]) // uppercase → invalid
        .assert()
        .failure();
    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args(["token", "issue", "has space"])
        .assert()
        .failure();
}

#[test]
fn revoke_removes_user_line_and_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let env = [("KB_HOME", tmp.path().to_path_buf())];
    Command::cargo_bin("kb")
        .unwrap()
        .envs(env.clone())
        .args(["token", "issue", "alice"])
        .assert()
        .success();
    Command::cargo_bin("kb")
        .unwrap()
        .envs(env.clone())
        .args(["token", "issue", "bob"])
        .assert()
        .success();

    Command::cargo_bin("kb")
        .unwrap()
        .envs(env)
        .args(["token", "revoke", "alice"])
        .assert()
        .success();

    let body = std::fs::read_to_string(tmp.path().join("config").join("tokens")).unwrap();
    assert!(
        !body.lines().any(|l| l.starts_with("alice:")),
        "alice should be gone: {body:?}"
    );
    assert!(
        body.lines().any(|l| l.starts_with("bob:")),
        "bob should remain: {body:?}"
    );
}
