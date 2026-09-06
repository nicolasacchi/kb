//! PRR-N12 (N2) — `kb-code scip run` against a REAL daemon.
//!
//! `--dry-run` must print the configured argv WITHOUT ever spawning it (the
//! configured `command[0]` here — `this-binary-does-not-exist-xyz` — would
//! fail to spawn if `scip run` ever actually tried; a passing dry-run test
//! is itself proof nothing was spawned). `--all` with no `[[scip.repos]]`
//! entries anywhere reports "none configured" and still exits success (an
//! empty batch is not a failure).

use crate::common::{boot, git};
use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, RepoEntry, ScipRepoEntry, ScipSection};
use predicates::str::contains;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), b"fn f() -> i32 {\n    1\n}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

/// Same shape as `common::boot`, plus an explicit `[scip]` section — the
/// shared helper has no scip knob (most callers don't need one).
async fn boot_with_scip(
    repo_dir: &std::path::Path,
    repo_name: &str,
    scip: ScipSection,
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
        scip,
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
async fn dry_run_prints_the_configured_argv_without_spawning() {
    let repo_tmp = fixture_repo();
    let scip = ScipSection {
        repos: vec![ScipRepoEntry {
            name: "fixture".to_string(),
            command: vec![
                "this-binary-does-not-exist-xyz".to_string(),
                "scip".to_string(),
            ],
            output: "index.scip".to_string(),
            langs: vec!["rust".to_string()],
        }],
    };
    let (_tmp, url, task) = boot_with_scip(repo_tmp.path(), "fixture", scip).await;

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "scip",
            "run",
            "--repo",
            "fixture",
            "--dry-run",
            "--json",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    let results = body["results"].as_array().expect("results array");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["repo"], "fixture");
    assert_eq!(results[0]["ok"], true);
    assert_eq!(results[0]["dry_run"], true);
    assert_eq!(
        results[0]["argv"].as_array().unwrap().clone(),
        vec!["this-binary-does-not-exist-xyz", "scip"]
    );
    assert_eq!(results[0]["output"], "index.scip");

    // Human mode: same argv, as readable text — and still no spawn (the
    // command would fail loudly if `scip run` actually tried it).
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "scip",
            "run",
            "--repo",
            "fixture",
            "--dry-run",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("this-binary-does-not-exist-xyz"))
        .stdout(contains("dry-run"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_with_no_scip_repos_configured_reports_none_and_succeeds() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["scip", "run", "--all", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("no repos with a [[scip.repos]] entry configured"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_repo_fails_cleanly() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["scip", "run", "--repo", "does-not-exist", "--daemon", &url])
        .assert()
        .failure()
        .stderr(contains("no such repo"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_repo_with_empty_command_reports_a_per_repo_error() {
    let repo_tmp = fixture_repo();
    let scip = ScipSection {
        repos: vec![ScipRepoEntry {
            name: "fixture".to_string(),
            command: Vec::new(),
            output: "index.scip".to_string(),
            langs: vec!["rust".to_string()],
        }],
    };
    let (_tmp, url, task) = boot_with_scip(repo_tmp.path(), "fixture", scip).await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["scip", "run", "--all", "--daemon", &url])
        .assert()
        .failure()
        .stdout(contains("empty command"));

    task.abort();
}
