//! `kb-code refs`/`tree`/`cat` — W1.3 smoke tests. These are DIRECT library
//! calls against `kb_code_server::git` (`--repo`, no daemon), unlike
//! `identity.rs`'s daemon round-trip — see the module doc on
//! `kb_code_cli::main` / `kb_code_server::git`. The exhaustive
//! fixture-matrix coverage (linked worktrees, submodules, shallow clones,
//! detached HEAD, size caps, ...) lives in `kb-code-server`'s own
//! `src/git/tests.rs`; these just prove the CLI wiring works end-to-end.

use crate::common::git;
use assert_cmd::Command;
use predicates::str::contains;

/// A tiny fixture repo: one commit on `main` with a root file and a
/// subdirectory, plus a lightweight tag.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("hello.txt"), b"hello world\n").unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub").join("nested.txt"), b"nested\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    git(dir, &["tag", "v1"]);
    tmp
}

#[test]
fn refs_lists_branch_and_tag() {
    let tmp = fixture_repo();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["refs", "--repo", tmp.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("main"))
        .stdout(contains("v1"))
        .stdout(contains("(HEAD)"));
}

#[test]
fn tree_lists_root_at_head_human_and_json() {
    let tmp = fixture_repo();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["tree", "--repo", tmp.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("hello.txt"))
        .stdout(contains("sub"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["tree", "--repo", tmp.path().to_str().unwrap(), "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let entries: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    let arr = entries.as_array().expect("json array");
    assert!(arr
        .iter()
        .any(|e| e["name"] == "hello.txt" && e["kind"] == "file"));
    assert!(arr.iter().any(|e| e["name"] == "sub" && e["kind"] == "dir"));
}

#[test]
fn tree_at_a_ref_and_nested_path() {
    let tmp = fixture_repo();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "tree",
            "sub",
            "--repo",
            tmp.path().to_str().unwrap(),
            "--ref",
            "v1",
        ])
        .assert()
        .success()
        .stdout(contains("nested.txt"));
}

#[test]
fn cat_prints_raw_bytes() {
    let tmp = fixture_repo();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["cat", "hello.txt", "--repo", tmp.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout("hello world\n");
}

#[test]
fn cat_missing_path_fails_cleanly() {
    let tmp = fixture_repo();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "cat",
            "no-such-file.txt",
            "--repo",
            tmp.path().to_str().unwrap(),
        ])
        .assert()
        .failure()
        .stderr(contains("no-such-file.txt"));
}

#[test]
fn refs_on_non_repo_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["refs", "--repo", tmp.path().to_str().unwrap()])
        .assert()
        .failure();
}
