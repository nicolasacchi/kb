//! W3.2 — end-to-end HTTP test for `GET /api/join/commit`, against a real
//! daemon booted via `serve_on_random_port_with_paths` over a real git
//! fixture repo. Mirrors `tests/search_routes.rs`'s own conventions
//! (`boot_with_repos`-style helper, `SERIAL` guard).
//!
//! W3.6 adds the `POST /api/backfill` smoke tests at the bottom of this
//! file — same `boot_with_repo` fixture, since the precompute is built
//! directly on the same join ladder these `join/commit` tests already
//! exercise; the precompute's own full arm/counting matrix (mock kb daemon
//! included) lives in `kb_code_server::join::backfill`'s unit tests.
//!
//! `[kb_daemon] enabled = false` is set EXPLICITLY on every boot here —
//! this test must never risk reaching a real kb daemon that happens to be
//! listening on the default `127.0.0.1:4000` on the machine running the
//! suite. The trailer arm (pure local) still fully exercises the route with
//! federation off; the ladder's own much larger arm matrix (mock kb daemon
//! included) lives in `kb_code_server::join::ladder`'s unit tests.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::git;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        // Never risk a real network call to an operator's actual kb daemon
        // during this suite — see the module doc.
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_commit_resolves_the_trailer_arm_over_http() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "test@example.com"]);
    git(&dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), b"hello\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "the subject\n\nKb-Session: sess-http-trailer\n",
        ],
    );
    let sha = String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/join/commit"))
        .query(&[("repo", "r"), ("sha", sha.as_str())])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "join/1");
    assert_eq!(body["confidence"], "trailer");
    assert_eq!(body["via"], "commit-trailer");
    assert_eq!(body["session_id"], "sess-http-trailer");
    assert_eq!(body["sha"], sha);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_commit_rejects_a_malformed_sha_with_400() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);

    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/join/commit"))
        .query(&[("repo", "r"), ("sha", "zz")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_commit_reports_404_for_an_unknown_repo() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);

    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/join/commit"))
        .query(&[("repo", "does-not-exist"), ("sha", "deadbeef")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

// --- W3.6 — POST /api/backfill ---------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_route_resolves_the_trailer_arm_and_reports_stats() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "test@example.com"]);
    git(&dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), b"hello\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "the subject\n\nKb-Session: sess-backfill-http\n",
        ],
    );

    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/backfill"))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "join/backfill/1");
    assert_eq!(body["repo"], "r");
    assert_eq!(body["total"], 1);
    // `[kb_daemon] enabled = false` (see `boot_with_repo`) — the up-front
    // commit-map fetch fails, so this stays a DEGRADED (not failed) run;
    // the trailer arm still resolves purely locally.
    assert_eq!(body["degraded"], true);
    let buckets = body["resolved_by_confidence"].as_array().unwrap();
    let trailer = buckets
        .iter()
        .find(|b| b["confidence"] == "trailer")
        .unwrap();
    assert_eq!(trailer["count"], 1);
    assert_eq!(body["newly_cached"], 1);

    // A second call re-uses the now-warm cache — no new cache writes.
    let resp2 = client
        .post(format!("{base}/api/backfill"))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap();
    let body2: serde_json::Value = resp2.json().await.unwrap();
    assert_eq!(body2["newly_cached"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backfill_route_reports_404_for_an_unknown_repo() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);

    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/backfill"))
        .query(&[("repo", "does-not-exist")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn on_boot_backfill_precomputes_before_any_explicit_call() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "test@example.com"]);
    git(&dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), b"hello\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            "the subject\n\nKb-Session: sess-on-boot\n",
        ],
    );

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "r".to_string(),
            path: dir.clone(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        backfill: kb_code_server::config::BackfillSection {
            depth: "all".to_string(),
            on_boot: true,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let db_path = paths.state.join("index.db");
    let (_addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");

    let sha = git_out(&dir, &["rev-parse", "HEAD"]);

    // A SEPARATE, read-only `Store` handle on the SAME sqlite file (WAL
    // mode supports concurrent readers) — deliberately NOT another
    // `POST /api/backfill` call, which would itself resolve+cache the
    // commit and make the assertion vacuously true regardless of whether
    // `[backfill] on_boot` actually ran. `upsert_repo` runs synchronously
    // during boot (before `serve_on_random_port_with_paths` even returns),
    // so `repo_id` is available immediately; only the `commit_sessions` row
    // (written by the fire-and-forget on-boot task) needs polling.
    let store = kb_code_server::store::Store::open(&db_path).unwrap();
    let repo_id = store
        .repo_id("r")
        .unwrap()
        .expect("repo registered at boot");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut found = None;
    while std::time::Instant::now() < deadline {
        if let Some(row) = store.get_commit_session(repo_id, &sha).unwrap() {
            found = Some(row);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let row = found.expect("the on-boot backfill should have written a commit_sessions row");
    assert_eq!(row.confidence, "trailer");
    assert_eq!(row.session_id.as_deref(), Some("sess-on-boot"));
}
