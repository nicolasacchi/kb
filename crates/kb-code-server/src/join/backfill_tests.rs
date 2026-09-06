//! W3.6 — the backfill precompute's own test matrix. Mirrors `join::
//! ladder_tests`'s conventions (a REAL fixture git repo + a MOCK kb daemon,
//! never the real `:4000` production daemon) but kept local to this file
//! since `ladder_tests`' own helpers are private to that module.

use super::*;
use crate::join::kb_client::test_support::{daemon_cfg, mock_kb_server};
use crate::store::CommitSessionRow;
use axum::extract::Query;
use axum::routing::get;
use axum::{Json, Router};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

// --- fixture plumbing (mirrors `join::ladder_tests`) -----------------------

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git -C {} {:?} failed: {}",
        dir.display(),
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

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

fn init_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    tmp
}

/// One commit with a controlled author date and an arbitrary raw message —
/// each call touches a FRESH file (`seq`) so every commit is non-empty.
fn commit(dir: &Path, seq: usize, message: &str, author_unix: i64) -> String {
    std::fs::write(dir.join(format!("f{seq}.txt")), b"content\n").unwrap();
    git(dir, &["add", "-A"]);
    let date = format!("{author_unix} +0000");
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .status()
        .expect("git commit runs");
    assert!(status.success());
    git_out(dir, &["rev-parse", "HEAD"])
}

fn repo_entry(name: &str, path: &Path) -> RepoEntry {
    RepoEntry {
        name: name.to_string(),
        path: path.to_path_buf(),
    }
}

fn store_with_repo(repo: &RepoEntry) -> (tempfile::TempDir, Arc<Store>, i64) {
    let tmp = tempfile::tempdir().unwrap();
    // `Arc`-wrapped — `backfill_repo`/`resolve_commit` now take `&Arc<Store>`
    // (2026-08-31 incident fix: must be able to hand the store off to
    // `run_blocking`'s blocking pool).
    let store = Arc::new(Store::open(&tmp.path().join("index.db")).unwrap());
    let repo_id = store
        .upsert_repo(&repo.name, &repo.path.to_string_lossy())
        .unwrap();
    (tmp, store, repo_id)
}

fn kb_client_unreachable() -> KbClient {
    KbClient::new(daemon_cfg("http://127.0.0.1:0".to_string()))
}

/// An empty-but-REACHABLE mock — every `by-commit`/`commit-map` call
/// succeeds with no matches (mirrors `join::ladder_tests::empty_router`).
fn empty_router() -> Router {
    Router::new()
        .route(
            "/api/sessions/by-commit",
            get(|| async { Json(serde_json::json!({ "matches": [] })) }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        )
}

fn commit_match_json(session_id: &str, sha_full: &str, display_name: &str) -> serde_json::Value {
    serde_json::json!({
        "kb": "memory",
        "session_id": session_id,
        "artifact_id": "art-1",
        "kind": "commit",
        "sha": sha_full,
        "sha_full": sha_full,
        "resolved": true,
        "subject": display_name,
        "display_name": display_name,
        "started_at": 1_700_000_000
    })
}

/// A mock kb daemon whose `by-commit` response depends on the REQUESTED
/// sha — `known` maps a sha to the `commit_match_json` it should resolve
/// to; any other sha gets an empty match list. `commit-map` is always
/// empty (these tests only exercise arm 2's exact by-commit path).
fn by_commit_lookup_router(known: HashMap<String, serde_json::Value>) -> Router {
    Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let known = known.clone();
                async move {
                    let sha = q.get("sha").cloned().unwrap_or_default();
                    let matches: Vec<serde_json::Value> =
                        known.get(&sha).cloned().into_iter().collect();
                    Json(serde_json::json!({ "matches": matches }))
                }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        )
}

fn bucket_count(stats: &BackfillStats, confidence: &str) -> usize {
    stats
        .resolved_by_confidence
        .iter()
        .find(|b| b.confidence == confidence)
        .map(|b| b.count)
        .unwrap_or(0)
}

// --- stats shape ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_count_exactly_across_ten_commits() {
    let tmp = init_repo();
    let dir = tmp.path();
    let base = 1_700_000_000i64;

    // 3 trailer commits (arm 1) + 4 commits kb "recorded" (resolved via
    // arm 2's exact by-commit match) + 3 commits kb has never heard of
    // (arm 2 misses, no commit-map row either — "none").
    let mut trailer_shas = Vec::new();
    for i in 0..3 {
        trailer_shas.push(commit(
            dir,
            i,
            &format!("trailer commit {i}\n\nKb-Session: sess-trailer-{i}\n"),
            base + i as i64,
        ));
    }
    let mut recorded_shas = Vec::new();
    for i in 3..7 {
        recorded_shas.push(commit(
            dir,
            i,
            &format!("recorded commit {i}"),
            base + i as i64,
        ));
    }
    for i in 7..10 {
        commit(dir, i, &format!("unknown commit {i}"), base + i as i64);
    }

    let repo = repo_entry("r", dir);
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);

    let mut known = HashMap::new();
    for (idx, sha) in recorded_shas.iter().enumerate() {
        known.insert(
            sha.clone(),
            commit_match_json(&format!("sess-recorded-{idx}"), sha, "recorded"),
        );
    }
    let router = by_commit_lookup_router(known);
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let stats = backfill_repo(&repo, repo_id, None, &store, &kb_client)
        .await
        .unwrap();

    assert_eq!(stats.schema, SCHEMA);
    assert_eq!(stats.repo, "r");
    assert_eq!(stats.total, 10);
    assert_eq!(bucket_count(&stats, "trailer"), 3);
    assert_eq!(bucket_count(&stats, "exact"), 4);
    assert_eq!(bucket_count(&stats, "fuzzy"), 0);
    assert_eq!(bucket_count(&stats, "none"), 3);
    assert_eq!(stats.newly_cached, 10);
    assert_eq!(stats.upgraded, 0);
    assert!(!stats.degraded);

    // Every walked sha actually landed a cache row.
    for sha in &trailer_shas {
        let row = store.get_commit_session(repo_id, sha).unwrap().unwrap();
        assert_eq!(row.confidence, "trailer");
    }
}

// --- depth filtering ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn depth_filters_commits_older_than_the_window() {
    let tmp = init_repo();
    let dir = tmp.path();
    let now = chrono::Utc::now().timestamp();
    let day = 86_400i64;

    commit(dir, 0, "5 days ago", now - 5 * day);
    commit(dir, 1, "3 days ago", now - 3 * day);
    let recent_a = commit(dir, 2, "1 day ago", now - day);
    let recent_b = commit(dir, 3, "just now", now);

    let repo = repo_entry("r", dir);
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let (addr, _server) = mock_kb_server(empty_router()).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let stats = backfill_repo(
        &repo,
        repo_id,
        Some(Duration::from_secs(2 * 86_400)),
        &store,
        &kb_client,
    )
    .await
    .unwrap();

    assert_eq!(
        stats.total, 2,
        "only the two commits within the 2-day window should be walked"
    );
    assert!(store
        .get_commit_session(repo_id, &recent_a)
        .unwrap()
        .is_some());
    assert!(store
        .get_commit_session(repo_id, &recent_b)
        .unwrap()
        .is_some());

    // The full-history run (depth = None / "all") sees all four.
    let (_tmp2, store2, repo_id2) = store_with_repo(&repo);
    let full = backfill_repo(&repo, repo_id2, None, &store2, &kb_client)
        .await
        .unwrap();
    assert_eq!(full.total, 4);
}

// --- cache warmth across runs ------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_run_reports_zero_newly_cached() {
    let tmp = init_repo();
    let dir = tmp.path();
    let base = 1_700_000_000i64;
    for i in 0..3 {
        commit(
            dir,
            i,
            &format!("c{i}\n\nKb-Session: sess-{i}\n"),
            base + i as i64,
        );
    }
    let repo = repo_entry("r", dir);
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let (addr, _server) = mock_kb_server(empty_router()).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let first = backfill_repo(&repo, repo_id, None, &store, &kb_client)
        .await
        .unwrap();
    assert_eq!(first.total, 3);
    assert_eq!(first.newly_cached, 3);

    let second = backfill_repo(&repo, repo_id, None, &store, &kb_client)
        .await
        .unwrap();
    assert_eq!(second.total, 3);
    assert_eq!(
        second.newly_cached, 0,
        "every commit already has a fresh (permanent, trailer-confidence) cache row"
    );
    assert_eq!(second.upgraded, 0);
}

// --- upgrade counting ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upgraded_counts_a_stale_none_row_that_now_resolves() {
    let tmp = init_repo();
    let dir = tmp.path();
    let sha = commit(dir, 0, "plain commit, no trailer", 1_700_000_000);
    let repo = repo_entry("r", dir);
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);

    // Run 1: kb is reachable but has never heard of this sha — resolves to
    // "none".
    let (addr1, _server1) = mock_kb_server(empty_router()).await;
    let kb_client_1 = KbClient::new(daemon_cfg(format!("http://{addr1}")));
    let first = backfill_repo(&repo, repo_id, None, &store, &kb_client_1)
        .await
        .unwrap();
    assert_eq!(first.newly_cached, 1);
    assert_eq!(bucket_count(&first, "none"), 1);
    assert_eq!(first.upgraded, 0);

    // Simulate the TTL (`ladder::TTL_SECS`) having elapsed on that "none"
    // row, so a re-run is willing to recompute it rather than serving the
    // still-fresh cached "none" verbatim.
    let stale_row = CommitSessionRow {
        confidence: "none".to_string(),
        via: "no-match".to_string(),
        session_id: None,
        kb: None,
        display_name: None,
        started_at: None,
        resolved_at: chrono::Utc::now().timestamp() - ladder::TTL_SECS - 1,
    };
    store
        .upsert_commit_session(repo_id, &sha, &stale_row)
        .unwrap();

    // Run 2: a DIFFERENT mock — kb has since learned about the sha.
    let mut known = HashMap::new();
    known.insert(
        sha.clone(),
        commit_match_json("sess-learned-later", &sha, "learned later"),
    );
    let (addr2, _server2) = mock_kb_server(by_commit_lookup_router(known)).await;
    let kb_client_2 = KbClient::new(daemon_cfg(format!("http://{addr2}")));
    let second = backfill_repo(&repo, repo_id, None, &store, &kb_client_2)
        .await
        .unwrap();

    assert_eq!(second.total, 1);
    assert_eq!(
        second.newly_cached, 0,
        "a row already existed (the stale none row) — this is an upgrade, not a fresh cache"
    );
    assert_eq!(second.upgraded, 1);
    assert_eq!(bucket_count(&second, "exact"), 1);

    let row = store.get_commit_session(repo_id, &sha).unwrap().unwrap();
    assert_eq!(row.confidence, "exact");
    assert_eq!(row.session_id.as_deref(), Some("sess-learned-later"));
}

// --- degraded (unreachable federated kb daemon) ------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unreachable_daemon_gives_trailer_only_stats_and_degraded_flag() {
    let tmp = init_repo();
    let dir = tmp.path();
    let base = 1_700_000_000i64;
    let trailer_a = commit(dir, 0, "t0\n\nKb-Session: sess-a\n", base);
    let trailer_b = commit(dir, 1, "t1\n\nKb-Session: sess-b\n", base + 1);
    let plain = commit(dir, 2, "plain, no trailer", base + 2);

    let repo = repo_entry("r", dir);
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let kb_client = kb_client_unreachable();

    let stats = backfill_repo(&repo, repo_id, None, &store, &kb_client)
        .await
        .unwrap();

    assert!(stats.degraded, "an unreachable kb daemon must set degraded");
    assert_eq!(stats.total, 3);
    assert_eq!(bucket_count(&stats, "trailer"), 2);
    assert_eq!(bucket_count(&stats, "none"), 1);
    assert_eq!(stats.newly_cached, 3);

    let a = store
        .get_commit_session(repo_id, &trailer_a)
        .unwrap()
        .unwrap();
    assert_eq!(a.confidence, "trailer");
    assert_eq!(a.session_id.as_deref(), Some("sess-a"));
    let b = store
        .get_commit_session(repo_id, &trailer_b)
        .unwrap()
        .unwrap();
    assert_eq!(b.confidence, "trailer");
    assert_eq!(b.session_id.as_deref(), Some("sess-b"));
    let p = store.get_commit_session(repo_id, &plain).unwrap().unwrap();
    assert_eq!(p.confidence, "none");
    assert_eq!(p.via, "kb-unreachable");
}

// --- walk_commits (git subprocess) ------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn walk_commits_reports_newest_first() {
    let tmp = init_repo();
    let dir = tmp.path();
    let sha1 = commit(dir, 0, "first", 1_700_000_000);
    let sha2 = commit(dir, 1, "second", 1_700_000_100);
    let shas = walk_commits(dir, None).await.unwrap();
    assert_eq!(shas, vec![sha2, sha1]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn walk_commits_errors_cleanly_on_a_non_repo_path() {
    let tmp = tempfile::tempdir().unwrap();
    let err = walk_commits(tmp.path(), None).await.unwrap_err();
    assert!(matches!(
        err,
        BackfillError::GitLog(_) | BackfillError::Spawn(_)
    ));
}
