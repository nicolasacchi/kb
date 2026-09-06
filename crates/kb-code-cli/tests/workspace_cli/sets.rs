//! `kb-code set` / `kb-code pack --set` — Phase E3 CLI smoke tests against
//! a REAL running kb-code daemon. Mirrors `tests/annotations.rs`'s own
//! `boot()`/fixture-repo conventions.

use crate::common::{boot, git};
use assert_cmd::Command;
use predicates::str::contains;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

/// Poll `GET /api/repos`'s `file_count` until the boot walk has indexed
/// `expected` files — `pack --set` reads the store's symbols/outline
/// tables, so a test asserting on its output must wait for this first
/// (same convention `tests/agentview_routes.rs` establishes server-side).
async fn wait_for_indexed(base: &str, repo: &str, expected: usize) {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    if entry["file_count"].as_u64().unwrap_or(0) as usize >= expected {
                        return;
                    }
                }
            }
        }
        assert!(std::time::Instant::now() < deadline, "indexing timed out");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_create_add_show_pack_delete_flow_round_trips_over_the_cli() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 2).await;

    // create
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "set",
            "create",
            "the ingest path",
            "--repo",
            "fixture",
            "-d",
            "how a request flows",
            "--span",
            "lib.rs:1-2",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("created set"));

    // list
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["set", "list", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("the ingest path"));

    // add
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "set",
            "add",
            "the ingest path",
            "main.rs",
            "--note",
            "entrypoint",
            "--repo",
            "fixture",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("added main.rs"));

    // show
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "set",
            "show",
            "the ingest path",
            "--repo",
            "fixture",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("lib.rs:1-2"))
        .stdout(contains("main.rs"))
        .stdout(contains("entrypoint"));

    // pack --set (NAME resolution + line/note annotation surfaced)
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "pack",
            "--set",
            "the ingest path",
            "--repo",
            "fixture",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("lib.rs :1-2"))
        .stdout(contains("entrypoint"))
        .stdout(contains("main.rs"));

    // rm ordinal 0 (lib.rs)
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "set",
            "rm",
            "the ingest path",
            "0",
            "--repo",
            "fixture",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("removed span 0"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "set",
            "show",
            "the ingest path",
            "--repo",
            "fixture",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("main.rs"));

    // delete --yes
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "set",
            "delete",
            "the ingest path",
            "--repo",
            "fixture",
            "--yes",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("deleted"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["set", "list", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("no reading sets"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_create_rejects_a_duplicate_name() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "set", "create", "dup", "--repo", "fixture", "--daemon", &url,
        ])
        .assert()
        .success();

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "set", "create", "dup", "--repo", "fixture", "--daemon", &url,
        ])
        .assert()
        .failure()
        .stderr(contains("409"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_rejects_paths_and_set_together_at_the_clap_layer() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "pack", "lib.rs", "--set", "x", "--repo", "fixture", "--daemon", &url,
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));

    task.abort();
}
