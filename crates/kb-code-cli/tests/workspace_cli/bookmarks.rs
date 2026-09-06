//! `kb-code bookmarks` / `kb-code bookmark` — Phase N CLI smoke tests
//! against a REAL running kb-code daemon. Mirrors `tests/sets.rs`.

use crate::common::{boot, git};
use assert_cmd::Command;
use predicates::str::contains;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bookmark_create_list_rm_by_id_and_mnemonic() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "bookmark",
            "lib.rs:1",
            "--repo",
            "fixture",
            "--mnemonic",
            "a",
            "--note",
            "entry",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("bookmarked lib.rs:1"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["bookmarks", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("lib.rs:1"))
        .stdout(contains("entry"));

    // rm by mnemonic
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["bookmark", "rm", "a", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("deleted bookmark"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["bookmarks", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("no bookmarks"));

    // create again, rm by id
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "bookmark", "lib.rs:2", "--repo", "fixture", "--daemon", &url, "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let id = body["id"].as_i64().unwrap().to_string();

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["bookmark", "rm", &id, "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("deleted bookmark"));

    task.abort();
}
