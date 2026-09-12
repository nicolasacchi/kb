//! `kb-code search <q>` — W2.4 CLI smoke tests for the unified
//! Search-Everywhere box against a REAL running kb-code daemon. Mirrors
//! `tests/search.rs`'s own `boot`/`fixture_repo`/`wait_for_json`
//! conventions (each e2e file in this crate duplicates this small helper
//! set rather than sharing one — see `search_routes.rs`'s doc in the
//! kb-code-server crate for the same rationale, followed here too).

use crate::common::{git, wait_for_json};
use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use predicates::str::contains;
use std::path::Path;

/// Never resolves to a live daemon — the sessions lane must degrade to
/// `unavailable_reason`, never hang, whether or not a real `kb` daemon
/// happens to be running on the machine these tests execute on.
const DEAD_KB_DAEMON: &str = "http://127.0.0.1:1";

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("gizmo_widget.rs"),
        b"fn known_symbol_name() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
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
        kb_daemon: KbDaemonSection {
            enabled: true,
            url: Some(DEAD_KB_DAEMON.to_string()),
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
async fn bare_query_dispatches_to_the_unified_box_with_headered_sections() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    // Wait for the background initial-index walk to have actually
    // populated the symbols lane — a bare section-count check races that
    // walk (every lane is always ATTEMPTED regardless of indexing state,
    // so the count alone is true on the very first poll).
    wait_for_json(
        &format!("{url}/api/search?repo=fixture&q=known_symbol"),
        std::time::Duration::from_secs(15),
        |b| {
            b["sections"].as_array().is_some_and(|s| s.len() == 6)
                && b["sections"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|s| s["lane"] == "symbols")
                    .is_some_and(|s| {
                        s["results"]
                            .as_array()
                            .is_some_and(|r| r.iter().any(|h| h["name"] == "known_symbol_name"))
                    })
        },
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search",
            "@known_symbol",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("== symbols =="))
        .stdout(contains("known_symbol_name"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search",
            "known_symbol",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    assert_eq!(body["sections"].as_array().unwrap().len(), 6);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_and_sessions_sections_render_as_unavailable_not_a_crash() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    wait_for_json(
        &format!("{url}/api/search/files?repo=fixture&q=gizmo"),
        std::time::Duration::from_secs(15),
        |b| b["hits"].as_array().is_some_and(|a| !a.is_empty()),
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["search", "gizmo", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("== semantic =="))
        .stdout(contains("(unavailable:"))
        .stdout(contains("== sessions =="));
    task.abort();
}

#[test]
fn no_query_and_no_subcommand_fails_with_a_helpful_message() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["search"])
        .assert()
        .failure()
        .stderr(contains("unified Search-Everywhere box"));
}

#[test]
fn lane_subcommands_still_work_unchanged_alongside_the_bare_query_form() {
    // A regression pin for the clap restructuring (optional bare query +
    // sibling `SearchCmd` subcommand): "files" as the first token after
    // "search" must still dispatch to the FILES lane subcommand, not be
    // parsed as a literal query string.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search",
            "files",
            "anything",
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}
