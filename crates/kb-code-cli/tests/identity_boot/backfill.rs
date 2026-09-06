//! `kb-code backfill` — W3.6 CLI smoke tests against a REAL running
//! kb-code daemon. Mirrors `tests/provenance.rs`'s own `boot()`/fixture
//! conventions; the precompute's full arm/counting matrix (mock kb daemon
//! included) lives in `kb_code_server::join::backfill`'s unit tests, and the
//! bare HTTP route is covered by `kb-code-server/tests/join_route.rs`.

use crate::common::git;
use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use predicates::str::contains;
use std::path::Path;

fn commit_with_message(dir: &Path, file: &str, contents: &str, message: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// Two commits — one trailer-stamped, one plain — enough for the precompute
/// CLI smoke tests.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);

    commit_with_message(
        dir,
        "a.rs",
        "fn a() {}\n",
        "the subject\n\nKb-Session: sess-backfill-cli\n",
    );
    commit_with_message(dir, "b.rs", "fn b() {}\n", "plain commit, no trailer");
    tmp
}

async fn boot(
    repo_dir: &Path,
    repo_name: &str,
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        // Never risk a real network call to an operator's own kb daemon —
        // the trailer arm resolves fully locally, which is all these smoke
        // tests need (mirrors `tests/provenance.rs`'s own posture).
        kb_daemon: kb_code_server::config::KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_reports_stats_for_one_repo() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    // Run the `--json` form FIRST — this is the boot's very first backfill,
    // so `newly_cached` must equal `total` (nothing was cached yet).
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["backfill", "--repo", "fixture", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    assert_eq!(body["schema"], "join/backfill/1");
    assert_eq!(body["repo"], "fixture");
    assert_eq!(body["total"], 2);
    assert_eq!(body["newly_cached"], 2);
    let buckets = body["resolved_by_confidence"].as_array().unwrap();
    let trailer = buckets
        .iter()
        .find(|b| b["confidence"] == "trailer")
        .unwrap();
    assert_eq!(trailer["count"], 1);

    // A second, human-readable call re-uses the now-warm cache.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["backfill", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("backfill · fixture"))
        .stdout(contains("2 commit(s) walked"))
        .stdout(contains("trailer"))
        .stdout(contains("newly_cached: 0"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_without_repo_runs_every_configured_repo() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["backfill", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    // A single configured repo still returns the bare object (not a
    // one-element array) — see `backfill_cmd`'s doc.
    assert_eq!(body["repo"], "fixture");
    assert_eq!(body["total"], 2);
    task.abort();
}

#[test]
fn backfill_unreachable_daemon_fails_cleanly() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "backfill",
            "--repo",
            "fixture",
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}
