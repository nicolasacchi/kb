//! `kb-code bench-search` — W5.5 e2e smoke test against a REAL running
//! kb-code daemon + a tiny fixture repo (3 queries — see the module doc in
//! `src/bench.rs` for the scoring rules this exercises). Mirrors
//! `tests/search_unified.rs`'s own `boot`/`fixture_repo`/`wait_for_json`
//! conventions (each e2e file in this crate duplicates this small helper
//! set rather than sharing one — see `search_routes.rs`'s doc in the
//! kb-code-server crate for the same rationale).

use crate::common::{git, wait_for_json};
use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use predicates::str::contains;
use std::path::Path;

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
    std::fs::write(dir.join("helper.rs"), b"fn helper() -> i32 {\n    2\n}\n").unwrap();
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
            url: DEAD_KB_DAEMON.to_string(),
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

/// 3 tiny queries — one files-lane hit, one symbols-lane hit, one query
/// that misses every lane (proves the scoring path handles a miss without
/// crashing, not just the happy path).
const QUERIES: &str = r##"
{"query": "@known_symbol", "expect_path": "gizmo_widget.rs", "expect_kind": "symbol"}
{"query": "#gizmo_widget.rs", "expect_path": "gizmo_widget.rs", "expect_kind": "filename"}
{"query": "totally_absent_needle_xyz", "expect_path": "does_not_exist.rs", "expect_kind": "error-string"}
"##;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bench_search_scores_recall_and_latency_against_a_live_daemon() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    // Wait for the background initial-index walk to populate the symbols
    // lane, same race guard `search_unified.rs`'s own test uses.
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

    let queries_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(queries_file.path(), QUERIES).unwrap();

    // --json: assert the shape and that the two winnable queries actually
    // won (deterministic against a fixed 3-file repo).
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "bench-search",
            "--queries",
            queries_file.path().to_str().unwrap(),
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

    assert_eq!(body["queries"], 3);
    // The symbols lane must have seen (and hit) the @known_symbol query.
    assert!(body["lanes"]["symbols"]["n"].as_u64().unwrap() >= 1);
    assert!(body["lanes"]["symbols"]["hit_at_1"].as_u64().unwrap() >= 1);
    // The files lane must have hit the #gizmo_widget.rs query.
    assert!(body["lanes"]["files"]["hit_at_1"].as_u64().unwrap() >= 1);
    // Overall recall@1 is < 1.0 — the third query is an intentional miss.
    let overall_recall_1 = body["overall"]["recall_at_1"].as_f64().unwrap();
    assert!(overall_recall_1 < 1.0, "got {overall_recall_1}");
    assert_eq!(body["overall"]["n"], 3);
    assert_eq!(body["per_query"].as_array().unwrap().len(), 3);

    // Human table: lane names + the ADR-6 governance note both present.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "bench-search",
            "--queries",
            queries_file.path().to_str().unwrap(),
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("symbols"))
        .stdout(contains("overall"))
        .stdout(contains("ADR-6"));

    task.abort();
}

#[test]
fn bench_search_reports_the_offending_line_on_malformed_queries() {
    let queries_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        queries_file.path(),
        "{\"query\":\"ok\",\"expect_path\":\"a\"}\nnope\n",
    )
    .unwrap();

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "bench-search",
            "--queries",
            queries_file.path().to_str().unwrap(),
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("line 2"));
}

#[test]
fn bench_search_fails_loudly_on_a_missing_queries_file() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "bench-search",
            "--queries",
            "/nonexistent/kb-code-bench-queries.jsonl",
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("nonexistent"));
}
