//! W3.2 — the join ladder's full arm/precedence/safety test matrix. Split
//! out of `ladder.rs` (`#[path = "ladder_tests.rs"] mod tests;`) purely for
//! file-size sanity; this IS `ladder`'s own `#[cfg(test)] mod tests`, not a
//! separate integration test crate (`super::*` reaches every private arm
//! helper).
//!
//! Every test uses a REAL fixture git repo (the `git`/`git_out` helpers
//! mirror `git/tests.rs`'s own convention) and a MOCK kb daemon
//! (`kb_client::test_support::mock_kb_server` — a real axum server on an
//! ephemeral loopback port) — never the real `:4000` production daemon.

use super::*;
use crate::join::kb_client::test_support::{daemon_cfg, mock_kb_server};
use crate::store::Store;
use axum::extract::Query;
use axum::routing::get;
use axum::{Json, Router};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// --- fixture plumbing -------------------------------------------------

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

/// One commit with a controlled author date and an arbitrary raw message
/// (subject + optional body, exactly as `git commit -m` would receive it).
fn commit(dir: &Path, file: &str, message: &str, author_unix: i64) -> String {
    std::fs::write(dir.join(file), b"content\n").unwrap();
    git(dir, &["add", file]);
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
    // `Arc`-wrapped (not a bare `Store`) — `resolve_commit` now takes
    // `&Arc<Store>` (2026-08-31 incident fix: it must be able to hand the
    // store off to `run_blocking`'s blocking pool).
    let store = Arc::new(Store::open(&tmp.path().join("index.db")).unwrap());
    let repo_id = store
        .upsert_repo(&repo.name, &repo.path.to_string_lossy())
        .unwrap();
    (tmp, store, repo_id)
}

fn kb_client_unreachable() -> KbClient {
    KbClient::new(daemon_cfg("http://127.0.0.1:0".to_string()))
}

fn kb_client_disabled() -> KbClient {
    KbClient::new(crate::config::KbDaemonSection {
        enabled: false,
        url: "http://127.0.0.1:0".to_string(),
        token_file: None,
        public_url: None,
    })
}

fn commit_match_json(
    session_id: &str,
    sha_full: &str,
    kb: &str,
    display_name: &str,
    started_at: i64,
) -> serde_json::Value {
    serde_json::json!({
        "kb": kb,
        "session_id": session_id,
        "artifact_id": "art-1",
        "kind": "commit",
        "sha": sha_full,
        "sha_full": sha_full,
        "resolved": true,
        "subject": display_name,
        "display_name": display_name,
        "started_at": started_at
    })
}

fn commit_map_row_json(
    session_id: &str,
    kb: &str,
    started_at: i64,
    repo_root: &str,
    subject: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kb": kb,
        "session_id": session_id,
        "artifact_id": "art-1",
        "started_at": started_at,
        "kind": "commit",
        "resolved": subject.is_some(),
        "repo_root": repo_root,
        "subject": subject
    })
}

/// A mock kb daemon serving fixed `by-commit` matches and `commit-map` rows
/// (both wrapped in the real response envelope shape).
fn mock_router(matches: Vec<serde_json::Value>, commit_map_rows: Vec<serde_json::Value>) -> Router {
    Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move || {
                let body = matches.clone();
                async move { Json(serde_json::json!({ "matches": body })) }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(move || {
                let body = commit_map_rows.clone();
                async move {
                    Json(serde_json::json!({ "commits": body, "limit": 500, "offset": 0 }))
                }
            }),
        )
}

fn empty_router() -> Router {
    mock_router(Vec::new(), Vec::new())
}

// --- arm 1: trailer -----------------------------------------------------

#[tokio::test]
async fn trailer_arm_resolves_locally_even_when_kb_is_unreachable() {
    let tmp = init_repo();
    let sha = commit(
        tmp.path(),
        "a.txt",
        "the subject\n\nKb-Session: sess-trailer\n",
        1_700_000_000,
    );
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let kb_client = kb_client_unreachable();

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.schema, SCHEMA);
    assert_eq!(attr.confidence, Confidence::Trailer);
    assert_eq!(attr.via, VIA_COMMIT_TRAILER);
    assert_eq!(attr.session_id.as_deref(), Some("sess-trailer"));
    assert_eq!(
        attr.kb, None,
        "unreachable daemon must not fabricate enrichment"
    );
    assert_eq!(attr.display_name, None);
    assert_eq!(attr.started_at, None);
    assert_eq!(attr.sha, sha);
}

#[tokio::test]
async fn trailer_arm_enriches_from_a_reachable_daemon() {
    let tmp = init_repo();
    let sha = commit(
        tmp.path(),
        "a.txt",
        "the subject\n\nKb-Session: sess-trailer\n",
        1_700_000_000,
    );
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let router = mock_router(
        vec![commit_match_json(
            "sess-trailer",
            &sha,
            "memory",
            "fixed the gizmo",
            1_700_000_500,
        )],
        Vec::new(),
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::Trailer);
    assert_eq!(attr.via, VIA_COMMIT_TRAILER);
    assert_eq!(attr.session_id.as_deref(), Some("sess-trailer"));
    assert_eq!(attr.kb.as_deref(), Some("memory"));
    assert_eq!(attr.display_name.as_deref(), Some("fixed the gizmo"));
    assert_eq!(attr.started_at, Some(1_700_000_500));
}

// --- arm 2: exact ---------------------------------------------------------

#[tokio::test]
async fn exact_arm_matches_kbs_by_commit_when_there_is_no_local_trailer() {
    let tmp = init_repo();
    let sha = commit(
        tmp.path(),
        "a.txt",
        "plain commit, no trailer",
        1_700_000_000,
    );
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let router = mock_router(
        vec![commit_match_json(
            "sess-exact",
            &sha,
            "memory",
            "the exact match",
            1_700_000_100,
        )],
        Vec::new(),
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::Exact);
    assert_eq!(attr.via, VIA_BY_COMMIT);
    assert_eq!(attr.session_id.as_deref(), Some("sess-exact"));
    assert_eq!(attr.kb.as_deref(), Some("memory"));
    assert_eq!(attr.display_name.as_deref(), Some("the exact match"));
    assert_eq!(attr.started_at, Some(1_700_000_100));
}

// --- precedence: trailer beats exact --------------------------------------

#[tokio::test]
async fn a_commit_matching_both_trailer_and_exact_resolves_trailer() {
    let tmp = init_repo();
    let sha = commit(
        tmp.path(),
        "a.txt",
        "the subject\n\nKb-Session: sess-trailer-wins\n",
        1_700_000_000,
    );
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    // kb's own by-commit response would ALSO resolve this exact sha — but
    // under a DIFFERENT session id, proving arm 1 wins outright rather than
    // arm 2 ever being allowed to override it.
    let router = mock_router(
        vec![commit_match_json(
            "sess-would-be-exact",
            &sha,
            "memory",
            "a different session entirely",
            1_700_000_100,
        )],
        Vec::new(),
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::Trailer);
    assert_eq!(attr.via, VIA_COMMIT_TRAILER);
    assert_eq!(attr.session_id.as_deref(), Some("sess-trailer-wins"));
}

// --- prefix disambiguation ------------------------------------------------

#[tokio::test]
async fn a_short_sha_prefix_is_disambiguated_locally_before_querying_kb() {
    let tmp = init_repo();
    let sha = commit(tmp.path(), "a.txt", "plain commit", 1_700_000_000);
    let short = sha[..8].to_string();
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);

    let received: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
    let received_clone = received.clone();
    let match_body = vec![commit_match_json(
        "sess-1",
        &sha,
        "memory",
        "hit",
        1_700_000_100,
    )];
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let received = received_clone.clone();
                let body = match_body.clone();
                async move {
                    *received.lock().unwrap() = q.get("sha").cloned().unwrap_or_default();
                    Json(serde_json::json!({ "matches": body }))
                }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &short, &store, &kb_client).await;
    assert_eq!(
        *received.lock().unwrap(),
        sha,
        "kb must be queried with the LOCALLY disambiguated full sha, not the short prefix"
    );
    assert_eq!(
        attr.sha, sha,
        "the attribution echoes back the canonical full sha"
    );
    assert_eq!(attr.confidence, Confidence::Exact);
}

// --- arm 5: repo-scoped time window + cross-repo safety ------------------

#[tokio::test]
async fn time_window_arm_matches_a_repo_aligned_session() {
    let tmp = init_repo();
    let author_time = 1_700_000_000;
    let sha = commit(tmp.path(), "a.txt", "plain commit", author_time);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let repo_root = repo.path.to_string_lossy().to_string();
    let router = mock_router(
        Vec::new(),
        vec![commit_map_row_json(
            "sess-window",
            "memory",
            author_time - 60, // session started a minute before the commit
            &repo_root,
            None,
        )],
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::Fuzzy);
    assert_eq!(attr.via, VIA_TIME_WINDOW);
    assert_eq!(attr.session_id.as_deref(), Some("sess-window"));
    assert_eq!(
        attr.display_name, None,
        "commit-map rows never carry a display_name"
    );
}

#[tokio::test]
async fn time_window_arm_rejects_a_time_overlapping_session_from_a_different_repo() {
    let repo_a = init_repo();
    let repo_b = init_repo();
    let author_time = 1_700_000_000;
    let sha = commit(repo_a.path(), "a.txt", "plain commit", author_time);
    let repo = repo_entry("repo-a", repo_a.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);

    // The ONLY candidate session in the commit-map feed overlaps the
    // commit's author-time perfectly, but its repo_root is repo B's — the
    // wrong repo must NEVER win, per the design's explicit safety rule.
    let wrong_repo_root = repo_b.path().to_string_lossy().to_string();
    let router = mock_router(
        Vec::new(),
        vec![commit_map_row_json(
            "sess-wrong-repo",
            "memory",
            author_time,
            &wrong_repo_root,
            None,
        )],
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(
        attr.confidence,
        Confidence::None,
        "a time-overlapping session from a DIFFERENT repo must never win"
    );
    assert_eq!(attr.via, VIA_NO_MATCH);
    assert_eq!(attr.session_id, None);
}

#[tokio::test]
async fn time_window_arm_ignores_a_session_outside_the_window() {
    let tmp = init_repo();
    let author_time = 1_700_000_000;
    let sha = commit(tmp.path(), "a.txt", "plain commit", author_time);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let repo_root = repo.path.to_string_lossy().to_string();
    // A session that started two days later — well outside the window.
    let router = mock_router(
        Vec::new(),
        vec![commit_map_row_json(
            "sess-too-late",
            "memory",
            author_time + 2 * 24 * 60 * 60,
            &repo_root,
            None,
        )],
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::None);
    assert_eq!(attr.via, VIA_NO_MATCH);
}

// --- arm 3: fuzzy subject --------------------------------------------------

#[tokio::test]
async fn fuzzy_subject_arm_matches_an_equal_resolved_subject_in_the_same_repo() {
    let tmp = init_repo();
    let sha = commit(
        tmp.path(),
        "a.txt",
        "fix the frobnicator race",
        1_700_000_000,
    );
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let repo_root = repo.path.to_string_lossy().to_string();
    let router = mock_router(
        Vec::new(),
        vec![commit_map_row_json(
            "sess-subject",
            "memory",
            1_600_000_000, // far outside the time-window arm's reach
            &repo_root,
            Some("fix the frobnicator race"),
        )],
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::Fuzzy);
    assert_eq!(attr.via, VIA_SUBJECT);
    assert_eq!(attr.session_id.as_deref(), Some("sess-subject"));
}

#[tokio::test]
async fn fuzzy_subject_arm_is_repo_scoped() {
    let repo_a = init_repo();
    let repo_b = init_repo();
    let sha = commit(
        repo_a.path(),
        "a.txt",
        "fix the frobnicator race",
        1_700_000_000,
    );
    let repo = repo_entry("repo-a", repo_a.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let wrong_root = repo_b.path().to_string_lossy().to_string();
    let router = mock_router(
        Vec::new(),
        vec![commit_map_row_json(
            "sess-wrong-repo",
            "memory",
            1_600_000_000,
            &wrong_root,
            Some("fix the frobnicator race"),
        )],
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(
        attr.confidence,
        Confidence::None,
        "matching subject in the WRONG repo must not win"
    );
}

// --- arm 4: squash ---------------------------------------------------------

#[tokio::test]
async fn squash_trailer_preserved_mid_body_resolves_exact() {
    let tmp = init_repo();
    // The LAST paragraph is ordinary prose (not a trailer block), so git's
    // official trailer parser (arm 1) does not recognize the buried
    // `Kb-Session:` line as a trailer at all — only the squash arm's
    // whole-body scan (arm 4) can find it.
    let message = "Squashed commit of the following:\n\n\
                    commit aaaa111\n\n    original fix\n\n    Kb-Session: sess-squashed\n\n\
                    this is not a trailer block, just prose describing the squash\n";
    let sha = commit(tmp.path(), "a.txt", message, 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let repo_root = repo.path.to_string_lossy().to_string();
    let router = mock_router(
        Vec::new(),
        vec![commit_map_row_json(
            "sess-squashed",
            "memory",
            1_699_999_000,
            &repo_root,
            Some("totally unrelated subject"),
        )],
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::Exact);
    assert_eq!(attr.via, VIA_SQUASH_TRAILER);
    assert_eq!(attr.session_id.as_deref(), Some("sess-squashed"));
    // Enriched from the commit-map row sharing that session id (kb +
    // started_at only — the feed carries no display_name).
    assert_eq!(attr.kb.as_deref(), Some("memory"));
    assert_eq!(attr.started_at, Some(1_699_999_000));
    assert_eq!(attr.display_name, None);
}

#[tokio::test]
async fn squash_trailer_arm_is_not_repo_scoped_a_preserved_trailer_is_its_own_proof() {
    let tmp = init_repo();
    let message = "Squashed commit\n\n    Kb-Session: sess-anywhere\n\n    trailing prose\n";
    let sha = commit(tmp.path(), "a.txt", message, 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    // No commit-map row at all — nothing to align against, and the arm
    // still fires (no enrichment, but the session id is still resolved).
    let (addr, _server) = mock_kb_server(empty_router()).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::Exact);
    assert_eq!(attr.via, VIA_SQUASH_TRAILER);
    assert_eq!(attr.session_id.as_deref(), Some("sess-anywhere"));
    assert_eq!(attr.kb, None);
    assert_eq!(attr.started_at, None);
}

#[tokio::test]
async fn squash_subject_containment_resolves_fuzzy() {
    let tmp = init_repo();
    let long_subject = "Squash merge: multiple fixes for the frobnicator subsystem";
    let sha = commit(tmp.path(), "a.txt", long_subject, 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let repo_root = repo.path.to_string_lossy().to_string();
    let router = mock_router(
        Vec::new(),
        vec![commit_map_row_json(
            "sess-contained",
            "memory",
            1_600_000_000,
            &repo_root,
            // A substring of the squash commit's subject — arm 3's strict
            // equality would miss this; arm 4's containment must not.
            Some("fixes for the frobnicator subsystem"),
        )],
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::Fuzzy);
    assert_eq!(attr.via, VIA_SQUASH_SUBJECT);
    assert_eq!(attr.session_id.as_deref(), Some("sess-contained"));
}

#[tokio::test]
async fn squash_subject_containment_rejects_a_too_short_match() {
    let tmp = init_repo();
    // Local subject is long enough to clear the guard on its OWN side (and
    // deliberately not EQUAL to the candidate's, so arm 3's exact equality
    // can't fire first) — only the candidate's own too-short subject must
    // be what rejects this containment match.
    let sha = commit(
        tmp.path(),
        "a.txt",
        "fix the wip thing today",
        1_700_000_000,
    );
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let repo_root = repo.path.to_string_lossy().to_string();
    let router = mock_router(
        Vec::new(),
        vec![commit_map_row_json(
            "sess-x",
            "memory",
            1_600_000_000,
            &repo_root,
            Some("wip"),
        )],
    );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(
        attr.confidence,
        Confidence::None,
        "a trivial short subject must not containment-match"
    );
}

// --- none: honest absence + degradation ------------------------------------

#[tokio::test]
async fn no_match_anywhere_resolves_none_with_the_no_match_label() {
    let tmp = init_repo();
    let sha = commit(tmp.path(), "a.txt", "an unremarkable commit", 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let (addr, _server) = mock_kb_server(empty_router()).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::None);
    assert_eq!(attr.via, VIA_NO_MATCH);
    assert_eq!(attr.session_id, None);
}

#[tokio::test]
async fn unreachable_daemon_degrades_to_none_past_the_trailer_arm() {
    let tmp = init_repo();
    let sha = commit(tmp.path(), "a.txt", "no trailer here", 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let kb_client = kb_client_unreachable();

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::None);
    assert_eq!(attr.via, VIA_KB_UNREACHABLE);
}

#[tokio::test]
async fn disabled_daemon_degrades_to_none_with_its_own_label() {
    let tmp = init_repo();
    let sha = commit(tmp.path(), "a.txt", "no trailer here", 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let kb_client = kb_client_disabled();

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::None);
    assert_eq!(attr.via, VIA_KB_DISABLED);
}

// --- caching + TTL upgrade --------------------------------------------------

#[tokio::test]
async fn a_second_resolve_is_served_from_the_cache_without_a_second_daemon_call() {
    let tmp = init_repo();
    let sha = commit(tmp.path(), "a.txt", "plain commit", 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_clone = calls.clone();
    let sha_for_body = sha.clone();
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move || {
                let calls = calls_clone.clone();
                let sha = sha_for_body.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({
                        "matches": [commit_match_json("sess-1", &sha, "memory", "hit", 1_700_000_100)]
                    }))
                }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let first = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    let second = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(first, second);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "an `exact` (permanent) resolution must be served from the commit_sessions cache on the second call"
    );
}

#[tokio::test]
async fn a_stale_none_row_upgrades_once_kb_learns_the_sha() {
    let tmp = init_repo();
    let sha = commit(tmp.path(), "a.txt", "plain commit", 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);

    let learned = Arc::new(AtomicBool::new(false));
    let learned_clone = learned.clone();
    let sha_for_body = sha.clone();
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move || {
                let learned = learned_clone.clone();
                let sha = sha_for_body.clone();
                async move {
                    if learned.load(Ordering::SeqCst) {
                        Json(serde_json::json!({
                            "matches": [commit_match_json("sess-later", &sha, "memory", "learned it", 1_700_000_900)]
                        }))
                    } else {
                        Json(serde_json::json!({ "matches": [] }))
                    }
                }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    let first = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(first.confidence, Confidence::None);
    assert_eq!(first.via, VIA_NO_MATCH);

    // Simulate the TTL having elapsed: backdate the cached row directly
    // (rather than sleeping 24h) and flip the mock to "kb now knows."
    let stale_row = CommitSessionRow {
        confidence: "none".to_string(),
        via: VIA_NO_MATCH.to_string(),
        session_id: None,
        kb: None,
        display_name: None,
        started_at: None,
        resolved_at: chrono::Utc::now().timestamp() - TTL_SECS - 1,
    };
    store
        .upsert_commit_session(repo_id, &sha, &stale_row)
        .unwrap();
    learned.store(true, Ordering::SeqCst);

    let second = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(second.confidence, Confidence::Exact);
    assert_eq!(second.via, VIA_BY_COMMIT);
    assert_eq!(second.session_id.as_deref(), Some("sess-later"));
}

#[tokio::test]
async fn a_fresh_none_row_is_not_upgraded_before_its_ttl_expires() {
    let tmp = init_repo();
    let sha = commit(tmp.path(), "a.txt", "plain commit", 1_700_000_000);
    let repo = repo_entry("r", tmp.path());
    let (_store_tmp, store, repo_id) = store_with_repo(&repo);

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_clone = calls.clone();
    let sha_for_body = sha.clone();
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move || {
                let calls = calls_clone.clone();
                let sha = sha_for_body.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Json(serde_json::json!({
                        "matches": [commit_match_json("sess-later", &sha, "memory", "would win if asked", 1_700_000_900)]
                    }))
                }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        );
    let (addr, _server) = mock_kb_server(router).await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));

    // Seed a FRESH none row directly (resolved_at = now), then resolve
    // again — the cache must win even though the daemon would now answer.
    let fresh_row = CommitSessionRow {
        confidence: "none".to_string(),
        via: VIA_NO_MATCH.to_string(),
        session_id: None,
        kb: None,
        display_name: None,
        started_at: None,
        resolved_at: chrono::Utc::now().timestamp(),
    };
    store
        .upsert_commit_session(repo_id, &sha, &fresh_row)
        .unwrap();

    let attr = resolve_commit(&repo, repo_id, &sha, &store, &kb_client).await;
    assert_eq!(attr.confidence, Confidence::None);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a fresh none row must be served from cache, never re-querying kb"
    );
}

// --- golden join/1 shape ----------------------------------------------------

#[test]
fn golden_join1_shape_full_attribution() {
    let attr = Attribution {
        schema: SCHEMA,
        confidence: Confidence::Exact,
        via: VIA_BY_COMMIT.to_string(),
        session_id: Some("sess-1".to_string()),
        kb: Some("memory".to_string()),
        display_name: Some("fixed the gizmo".to_string()),
        started_at: Some(1_700_000_000),
        sha: "abc123full".to_string(),
    };
    let v = serde_json::to_value(&attr).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "schema": "join/1",
            "confidence": "exact",
            "via": "by-commit",
            "session_id": "sess-1",
            "kb": "memory",
            "display_name": "fixed the gizmo",
            "started_at": 1_700_000_000i64,
            "sha": "abc123full"
        })
    );
}

#[test]
fn golden_join1_shape_none_omits_every_optional_field() {
    let attr = Attribution::none("abc123full".to_string(), VIA_NO_MATCH);
    let v = serde_json::to_value(&attr).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "schema": "join/1",
            "confidence": "none",
            "via": "no-match",
            "sha": "abc123full"
        })
    );
    assert!(v.get("session_id").is_none());
    assert!(v.get("kb").is_none());
    assert!(v.get("display_name").is_none());
    assert!(v.get("started_at").is_none());
}

#[test]
fn confidence_as_str_and_parse_round_trip_every_variant() {
    for c in [
        Confidence::Trailer,
        Confidence::Exact,
        Confidence::Fuzzy,
        Confidence::None,
    ] {
        assert_eq!(Confidence::parse(c.as_str()), Some(c));
    }
    assert_eq!(Confidence::parse("bogus"), None);
}

// --- is_plausible_sha ------------------------------------------------------

#[test]
fn is_plausible_sha_accepts_4_to_64_hex_chars_only() {
    assert!(is_plausible_sha("abcd"));
    assert!(is_plausible_sha(&"a".repeat(64)));
    assert!(!is_plausible_sha("abc")); // too short
    assert!(!is_plausible_sha(&"a".repeat(65))); // too long
    assert!(!is_plausible_sha("xyz1234"));
    assert!(!is_plausible_sha(""));
}
