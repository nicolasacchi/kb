//! `kb-code repos`/`tree --daemon`/`symbols` — W1.6: `assert_cmd` against a
//! REAL running kb-code daemon, mirroring `tests/identity.rs`'s own `boot()`
//! and multi_thread-flavor convention (see that file's doc comment on why
//! `worker_threads = 2` is load-bearing: `assert_cmd`'s `.assert()`
//! synchronously blocks one worker thread waiting on the `kb-code` child
//! process, so the daemon's own serve task needs a SECOND thread to make
//! progress on).

use crate::common::{boot, git, wait_for_json};
use assert_cmd::Command;
use predicates::str::contains;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("lib.rs"),
        b"fn known_symbol_name() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub").join("nested.txt"), b"nested\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

/// Poll a daemon URL until `pred` holds over the parsed JSON body, or
/// `timeout` elapses — the initial background index (`bind_and_spawn`'s
/// item (a)) races with these tests, so `repos`/`symbols` need to wait for
/// it rather than assume it's already done.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repos_lists_the_configured_repo_with_counts() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    wait_for_json(
        &format!("{url}/api/repos"),
        std::time::Duration::from_secs(15),
        |b| b["repos"][0]["file_count"].as_u64().unwrap_or(0) >= 2,
    )
    .await;

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["repos", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    let repos = body["repos"].as_array().expect("repos array");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["name"].as_str(), Some("fixture"));
    assert!(repos[0]["file_count"].as_u64().unwrap_or(0) >= 2);

    let human = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["repos", "--daemon", &url])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(human).unwrap();
    assert!(text.contains("fixture"), "got: {text}");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_daemon_mode_lists_root_by_repo_name() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["tree", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("lib.rs"))
        .stdout(contains("sub"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["tree", "--daemon", &url, "--repo", "fixture", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    let entries = body["entries"].as_array().expect("entries array");
    assert!(entries
        .iter()
        .any(|e| e["name"] == "lib.rs" && e["kind"] == "file"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_daemon_mode_prints_raw_bytes() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["cat", "lib.rs", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout("fn known_symbol_name() -> i32 {\n    1\n}\n");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbols_query_finds_the_known_function() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    wait_for_json(
        &format!("{url}/api/symbols?repo=fixture&q=known_symbol"),
        std::time::Duration::from_secs(15),
        |b| b["matches"].as_array().is_some_and(|a| !a.is_empty()),
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "symbols",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--query",
            "known_symbol",
        ])
        .assert()
        .success()
        .stdout(contains("known_symbol_name"))
        .stdout(contains("lib.rs"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbols_path_form_lists_the_files_symbols() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    // The per-file form is a pure STORE lookup by blob hash (see
    // `routes::file`'s doc) — unlike `cat`/`tree` (always ODB reads), it
    // depends on the background initial-index walk having reached this
    // blob, so wait for it the same way the `--query` test above does.
    wait_for_json(
        &format!("{url}/api/symbols?repo=fixture&path=lib.rs&ref=HEAD"),
        std::time::Duration::from_secs(15),
        |b| b["symbols"].as_array().is_some_and(|a| !a.is_empty()),
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["symbols", "lib.rs", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("known_symbol_name"));
    task.abort();
}

#[test]
fn repos_unreachable_daemon_fails_cleanly() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["repos", "--daemon", "http://127.0.0.1:0"])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}

// Existing W1.3 offline invocations must keep working byte-for-byte with no
// `--daemon` flag — a regression guard for the dual-mode `--repo` reuse
// (`--repo` means a filesystem PATH offline, a configured NAME with
// `--daemon`).
#[test]
fn tree_without_daemon_flag_stays_in_process_offline() {
    let tmp = fixture_repo();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["tree", "--repo", tmp.path().to_str().unwrap()])
        .assert()
        .success()
        .stdout(contains("lib.rs"));
}
