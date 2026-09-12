//! `kb-code map`/`pack`/`defs`/`xrefs`/`similar`/`impact` — W5.1 + W5.2 CLI
//! smoke tests against a REAL running kb-code daemon. Mirrors `tests/
//! provenance.rs`'s own `boot()`/`wait_for_indexed`/fixture-repo
//! conventions — the ranking/parsing logic itself is already pin-tested at
//! the unit level inside `kb-code-server`'s own `agentview` module; these
//! tests prove each CLI verb reaches the daemon, parses the response, and
//! renders both human and `--json` output without panicking.

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

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);

    commit_with_message(
        dir,
        "known_file.rs",
        "fn distinctive_symbol() {}\nfn another_fn() {}\n",
        "the subject\n\nKb-Session: sess-agentview\n",
    );
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
        // none of these verbs' CORE behavior needs the federation lane.
        kb_daemon: kb_code_server::config::KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
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

/// Poll `GET /api/repos`'s `file_count` for `repo` until it reaches
/// `expected_files` — NOT `GET /api/file` succeeding: that route reads
/// working-tree bytes straight off disk, independent of `state.store`, so
/// it proves nothing about whether the background initial-index walk has
/// populated the `symbols` table yet (`map`/`pack`/`defs` all read it).
/// Mirrors `kb-code-server/tests/e2e_daemon.rs`'s established convention.
async fn wait_for_indexed(url: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(format!("{url}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    if count >= expected_files {
                        return;
                    }
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected repo {repo:?} to report file_count >= {expected_files} within the deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Companion wait for tests whose asserts read the `symbols` table
/// (`map`/`defs` both resolve through `store::Store::symbols_for_repo`):
/// polls `GET /api/repos`' `symbol_count` for `repo` until it reaches
/// `expected_symbols`. `wait_for_indexed`'s `file_count` reaching its target
/// only proves the file ROW landed — `upsert_file` and `replace_symbols` are
/// separate store calls, so there is a window where `file_count` is already
/// correct while the symbols derived from the last file are still being
/// written. Same mechanism/rationale as `kb-code-server/tests/
/// agentview_routes.rs`'s own `wait_for_symbols` (and `provenance_routes.rs`'s,
/// c9ac75f8) — a bounded 15s deadline that fails loudly rather than a bigger
/// blind sleep.
async fn wait_for_symbols(url: &str, repo: &str, expected_symbols: usize) {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(format!("{url}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["symbol_count"].as_u64().unwrap_or(0) as usize;
                    if count >= expected_symbols {
                        return;
                    }
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected repo {repo:?} to report symbol_count >= {expected_symbols} within the deadline"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn map_smoke_human_and_json() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;
    // The assertions below read the map outline, which is symbols-table
    // derived (`agentview::map::map_route` -> `symbols_for_repo`) — the
    // `distinctive_symbol`/`another_fn` pair in known_file.rs, 2 symbols.
    // `wait_for_indexed`'s file_count wait alone races the symbols write
    // (see wait_for_symbols's doc).
    wait_for_symbols(&url, "fixture", 2).await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["map", "--daemon", &url, "--repo", "fixture"])
        .assert()
        .success()
        .stdout(contains("distinctive_symbol"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args(["map", "--daemon", &url, "--repo", "fixture", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    assert_eq!(body["schema"], "map/1");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_smoke_human_and_json() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "pack",
            "known_file.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("known_file.rs"))
        .stdout(contains("sess-agentview"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "pack",
            "known_file.rs",
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
    assert_eq!(body["schema"], "pack/1");
    assert_eq!(body["files"][0]["content"]["status"], "full");
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defs_smoke_exact_match() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;
    // `defs` resolves through `agentview::xref::defs_route` ->
    // `symbols_for_repo`, same race as map_smoke above — without this the
    // symbol table can still be empty, so `distinctive_symbol` falls back
    // to the fuzzy/no-match path instead of an exact hit.
    wait_for_symbols(&url, "fixture", 2).await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "defs",
            "distinctive_symbol",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("exact"))
        .stdout(contains("known_file.rs"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "defs",
            "distinctive_symbol",
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
    assert_eq!(body["exact"], true);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn xrefs_smoke_word_boundary() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "xrefs",
            "distinctive_symbol",
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
    assert_eq!(body["schema"], "refs/1");
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "body: {body}");
    assert_eq!(results[0]["approximate"], true);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn similar_smoke_400s_when_semantic_disabled() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    wait_for_indexed(&url, "fixture", 1).await;

    // `get_json`'s `.error_for_status()` discards the 400 response BODY
    // (the daemon's own `{"error": "... semantic ..."}` text — see
    // `routes::search_semantic`/`agentview::similar::similar_route`'s
    // shared hint convention) before it ever reaches stderr, so this only
    // asserts what the CLI's existing error-mapping actually surfaces: a
    // non-zero exit and the raw HTTP status. The daemon-side 400-with-hint
    // itself is asserted directly over HTTP in `kb-code-server`'s own
    // `tests/agentview_routes.rs::similar_route_400s_with_a_hint_when_semantic_is_disabled`.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "similar",
            "known_file.rs:1-2",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .failure()
        .stderr(contains("400 Bad Request"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impact_smoke_human_and_json() {
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    // A second commit touching known_file.rs alongside a sibling — gives
    // impact a real (if tiny) co-change signal to report.
    std::fs::write(dir.join("sibling.rs"), "sibling").unwrap();
    commit_with_message(dir, "known_file.rs", "fn c() {}\n", "c2: touch both");

    let (_tmp, url, task) = boot(dir, "fixture").await;
    wait_for_indexed(&url, "fixture", 2).await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "impact",
            "known_file.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("approximate"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "impact",
            "known_file.rs",
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
    assert_eq!(body["schema"], "impact/1");
    assert_eq!(body["approximate"], true);
    task.abort();
}
