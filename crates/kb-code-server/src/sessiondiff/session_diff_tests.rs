//! W3.5 — `session_diff`'s full assembly test matrix. Split out of `mod.rs`
//! (`#[path = "session_diff_tests.rs"] mod tests;`), same file-size-sanity
//! convention `join::ladder` uses for its own arm matrix
//! (`ladder_tests.rs`) — this IS `sessiondiff`'s own `#[cfg(test)] mod
//! tests`, `super::*` reaches every private helper.
//!
//! Every test uses a REAL fixture git repo (the `git`/`git_out` helpers
//! mirror `join::ladder_tests`'s own convention), a MOCK kb daemon
//! (`join::kb_client::test_support::mock_kb_server`), and REAL transcript
//! JSONL fixture files indexed through the actual tail indexer
//! (`transcripts::indexer::tail_file`) — never a hand-built `Store` row, so
//! `read_turn_text`'s re-read-off-disk path is exercised exactly like
//! production.

use super::*;
use crate::join::kb_client::test_support::{daemon_cfg, mock_kb_server};
use axum::extract::Path as AxumPath;
use axum::routing::get;
use axum::{Json, Router};
use std::process::Command;

// --- fixture plumbing ------------------------------------------------------

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

/// One commit at a controlled author date (unix seconds), writing `file`
/// with `contents`. Returns the full commit sha.
fn commit_at(dir: &Path, file: &str, contents: &str, message: &str, author_unix: i64) -> String {
    std::fs::write(dir.join(file), contents).unwrap();
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

fn iso(base_unix: i64, offset_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(base_unix * 1000 + offset_ms)
        .unwrap()
        .to_rfc3339()
}

/// One `"user"` (top-level prompt) JSONL line.
fn user_line(uuid: &str, session_id: &str, ts: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","uuid":"{uuid}","parentUuid":null,"sessionId":"{session_id}","timestamp":"{ts}","isSidechain":false,"message":{{"role":"user","content":{text}}}}}"#,
        text = serde_json::to_string(text).unwrap()
    )
}

/// One `"assistant"` line carrying a single `tool_use` block.
#[allow(clippy::too_many_arguments)]
fn tool_use_line(
    uuid: &str,
    session_id: &str,
    ts: &str,
    tool_name: &str,
    file_path: &str,
    is_sidechain: bool,
) -> String {
    format!(
        r#"{{"type":"assistant","uuid":"{uuid}","parentUuid":null,"sessionId":"{session_id}","timestamp":"{ts}","isSidechain":{is_sidechain},"message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t-{uuid}","name":"{tool_name}","input":{{"file_path":{file_path}}}}}]}}}}"#,
        file_path = serde_json::to_string(file_path).unwrap()
    )
}

fn index_fixture(store: &Store, root: &Path, lines: &[String]) {
    let proj_dir = root.join("proj-a");
    std::fs::create_dir_all(&proj_dir).unwrap();
    let file = proj_dir.join("session1.jsonl");
    std::fs::write(&file, format!("{}\n", lines.join("\n"))).unwrap();
    crate::transcripts::indexer::tail_file(store, root, &file, "proj-a", true).unwrap();
}

fn commit_map_commit_json(
    sha: &str,
    repo_root: &Path,
    subject: &str,
    author: &str,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "commit",
        "sha": sha,
        "subject": subject,
        "resolved": true,
        "sha_full": sha,
        "repo_root": repo_root.display().to_string(),
        "author": author,
        "parents": 0,
        "trailers": []
    })
}

async fn mock_session_commits(
    session_id: &'static str,
    commits: Vec<serde_json::Value>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let router = Router::new().route(
        "/api/sessions/{session_id}/commits",
        get(move |AxumPath(sid): AxumPath<String>| {
            let commits = commits.clone();
            async move {
                assert_eq!(sid, session_id);
                Json(serde_json::json!({ "commits": commits }))
            }
        }),
    );
    mock_kb_server(router).await
}

// --- the main end-to-end scenario ------------------------------------------
//
// 2 prompts, 2 real fixture-repo commits, 1 uncommitted edit. Timeline (T =
// BASE, seconds; transcript turns in ms offsets from T):
//
//   prompt1        T+0ms
//   edit a.txt     T+1000ms   -> covered by commit1 (author T+2s = 2000ms)
//   [commit1 slots after the (now-empty, dropped) edit group, before prompt2]
//   prompt2        T+5000ms
//   edit c.txt     T+6000ms   -> covered by commit2 (author T+8s = 8000ms)
//   edit d.txt     T+7000ms   -> NEVER committed — the uncommitted evidence
//   [commit2 has no anchor after it — trails at the very end]
const BASE: i64 = 1_700_000_000;

#[tokio::test]
async fn full_scenario_two_prompts_two_commits_one_uncommitted_edit() {
    let repo_tmp = init_repo();
    let repo_dir = repo_tmp.path();
    let sha1 = commit_at(repo_dir, "a.txt", "hello\n", "commit one", BASE + 2);
    let sha2 = commit_at(repo_dir, "c.txt", "world\n", "commit two", BASE + 8);

    let store_tmp = tempfile::tempdir().unwrap();
    // `Arc`-wrapped — `session_diff` now takes `&Arc<Store>` (2026-08-31
    // incident fix: must be able to hand the store off to `run_blocking`'s
    // blocking pool).
    let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
    let transcripts_tmp = tempfile::tempdir().unwrap();
    let root = transcripts_tmp.path();

    let a_path = repo_dir.join("a.txt").display().to_string();
    let c_path = repo_dir.join("c.txt").display().to_string();
    let d_path = repo_dir.join("d.txt").display().to_string();

    let lines = vec![
        user_line("u1", "sess-1", &iso(BASE, 0), "add a widget"),
        tool_use_line("u2", "sess-1", &iso(BASE, 1_000), "Edit", &a_path, false),
        user_line("u3", "sess-1", &iso(BASE, 5_000), "now add a gadget"),
        tool_use_line("u4", "sess-1", &iso(BASE, 6_000), "Edit", &c_path, false),
        tool_use_line("u5", "sess-1", &iso(BASE, 7_000), "Write", &d_path, false),
    ];
    index_fixture(&store, root, &lines);

    let (addr, _server) = mock_session_commits(
        "sess-1",
        vec![
            commit_map_commit_json(&sha1, repo_dir, "commit one", "Ada"),
            commit_map_commit_json(&sha2, repo_dir, "commit two", "Ada"),
        ],
    )
    .await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));
    let repos = vec![RepoEntry {
        name: "myrepo".to_string(),
        path: repo_dir.to_path_buf(),
    }];

    let diff = session_diff("sess-1", None, &repos, &store, root, &kb_client)
        .await
        .unwrap();

    assert_eq!(diff.version, SCHEMA);
    assert_eq!(diff.session_id, "sess-1");
    assert_eq!(diff.display_name.as_deref(), Some("add a widget"));
    assert!(matches!(diff.commits_status, CommitsStatus::Ok));
    assert_eq!(diff.repos_touched, vec!["myrepo".to_string()]);
    assert_eq!(diff.totals.commits, 2);
    assert_eq!(diff.totals.commits_diffed, 2);
    assert_eq!(diff.totals.insertions, 2); // one line added in each commit
    assert_eq!(diff.totals.deletions, 0);
    // a.txt (commit1) + c.txt (commit2) + d.txt (still uncommitted) = 3.
    assert_eq!(diff.totals.files, 3);

    // Exact narrative order — see the const doc above for the derivation.
    assert_eq!(diff.segments.len(), 5, "got {:#?}", as_json(&diff));
    match &diff.segments[0] {
        Segment::Prompt { text, .. } => assert_eq!(text, "add a widget"),
        other => panic!("segment 0: expected Prompt, got {other:?}"),
    }
    match &diff.segments[1] {
        Segment::Commits { commits } => {
            assert_eq!(commits.len(), 1);
            assert_eq!(commits[0].sha, sha1);
            assert!(commits[0].diffed);
            assert_eq!(commits[0].insertions, 1);
        }
        other => panic!("segment 1: expected Commits([commit1]), got {other:?}"),
    }
    match &diff.segments[2] {
        Segment::Prompt { text, .. } => assert_eq!(text, "now add a gadget"),
        other => panic!("segment 2: expected Prompt, got {other:?}"),
    }
    match &diff.segments[3] {
        Segment::Uncommitted { files, turns } => {
            assert_eq!(files, &vec![d_path.clone()]);
            assert_eq!(turns.len(), 2, "both c.txt and d.txt turns are kept");
        }
        other => panic!("segment 3: expected Uncommitted([d.txt]), got {other:?}"),
    }
    match &diff.segments[4] {
        Segment::Commits { commits } => {
            assert_eq!(commits.len(), 1);
            assert_eq!(commits[0].sha, sha2);
        }
        other => panic!("segment 4: expected Commits([commit2]), got {other:?}"),
    }
}

fn as_json(diff: &SessionDiff) -> serde_json::Value {
    serde_json::to_value(diff).unwrap()
}

#[test]
fn segment_json_shape_is_internally_tagged_by_kind() {
    let prompt = Segment::Prompt {
        ts: 1,
        uuid: "u1".to_string(),
        text: "hi".to_string(),
    };
    let v = serde_json::to_value(&prompt).unwrap();
    assert_eq!(v["kind"], "prompt");
    assert_eq!(v["text"], "hi");

    let commits = Segment::Commits { commits: vec![] };
    let v = serde_json::to_value(&commits).unwrap();
    assert_eq!(v["kind"], "commits");
    assert!(v["commits"].as_array().unwrap().is_empty());

    let uncommitted = Segment::Uncommitted {
        files: vec!["a.txt".to_string()],
        turns: vec![],
    };
    let v = serde_json::to_value(&uncommitted).unwrap();
    assert_eq!(v["kind"], "uncommitted");
    assert_eq!(v["files"][0], "a.txt");
}

#[tokio::test]
async fn unknown_session_is_reported_cleanly() {
    let store_tmp = tempfile::tempdir().unwrap();
    // `Arc`-wrapped — `session_diff` now takes `&Arc<Store>` (2026-08-31
    // incident fix: must be able to hand the store off to `run_blocking`'s
    // blocking pool).
    let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
    let root_tmp = tempfile::tempdir().unwrap();

    // Populate a DIFFERENT session so the store isn't simply empty — proves
    // the scoping, not just "no rows anywhere."
    index_fixture(
        &store,
        root_tmp.path(),
        &[user_line("u1", "other-session", &iso(BASE, 0), "hi")],
    );

    let kb_client = KbClient::new(daemon_cfg("http://127.0.0.1:0".to_string()));
    let err = session_diff(
        "no-such-session",
        None,
        &[],
        &store,
        root_tmp.path(),
        &kb_client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SessionDiffError::UnknownSession(_)));
}

#[tokio::test]
async fn unknown_repo_filter_is_rejected() {
    let store_tmp = tempfile::tempdir().unwrap();
    // `Arc`-wrapped — `session_diff` now takes `&Arc<Store>` (2026-08-31
    // incident fix: must be able to hand the store off to `run_blocking`'s
    // blocking pool).
    let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
    let root_tmp = tempfile::tempdir().unwrap();
    index_fixture(
        &store,
        root_tmp.path(),
        &[user_line("u1", "sess-1", &iso(BASE, 0), "hi")],
    );
    let kb_client = KbClient::new(daemon_cfg("http://127.0.0.1:0".to_string()));

    let err = session_diff(
        "sess-1",
        Some("no-such-repo"),
        &[],
        &store,
        root_tmp.path(),
        &kb_client,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, SessionDiffError::UnknownRepo(_)));
}

#[tokio::test]
async fn unreachable_kb_daemon_degrades_only_the_commits_half() {
    let store_tmp = tempfile::tempdir().unwrap();
    // `Arc`-wrapped — `session_diff` now takes `&Arc<Store>` (2026-08-31
    // incident fix: must be able to hand the store off to `run_blocking`'s
    // blocking pool).
    let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
    let root_tmp = tempfile::tempdir().unwrap();
    let root = root_tmp.path();

    let repo_tmp = init_repo();
    let repo_dir = repo_tmp.path();
    let e_path = repo_dir.join("e.txt").display().to_string();

    let lines = vec![
        user_line("u1", "sess-2", &iso(BASE, 0), "prompt"),
        tool_use_line("u2", "sess-2", &iso(BASE, 1_000), "Edit", &e_path, false),
    ];
    index_fixture(&store, root, &lines);

    // Nothing listening on this port — a real "unreachable" failure.
    let kb_client = KbClient::new(daemon_cfg("http://127.0.0.1:0".to_string()));
    let repos = vec![RepoEntry {
        name: "myrepo".to_string(),
        path: repo_dir.to_path_buf(),
    }];

    let diff = session_diff("sess-2", None, &repos, &store, root, &kb_client)
        .await
        .unwrap();
    assert!(matches!(
        diff.commits_status,
        CommitsStatus::Degraded { .. }
    ));
    assert_eq!(diff.totals.commits, 0);
    assert_eq!(diff.totals.commits_diffed, 0);
    // With zero known commits, the edit turn's file can't be "covered" by
    // anything — it must still show up as uncommitted evidence.
    let uncommitted_segments: Vec<&Segment> = diff
        .segments
        .iter()
        .filter(|s| matches!(s, Segment::Uncommitted { .. }))
        .collect();
    assert_eq!(uncommitted_segments.len(), 1);
    match uncommitted_segments[0] {
        Segment::Uncommitted { files, .. } => assert_eq!(files, &vec![e_path]),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn disabled_kb_daemon_also_degrades_the_commits_half() {
    let store_tmp = tempfile::tempdir().unwrap();
    // `Arc`-wrapped — `session_diff` now takes `&Arc<Store>` (2026-08-31
    // incident fix: must be able to hand the store off to `run_blocking`'s
    // blocking pool).
    let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
    let root_tmp = tempfile::tempdir().unwrap();
    index_fixture(
        &store,
        root_tmp.path(),
        &[user_line("u1", "sess-3", &iso(BASE, 0), "hi")],
    );
    let kb_client = KbClient::new(crate::config::KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
        token_file: None,
        public_url: None,
    });

    let diff = session_diff("sess-3", None, &[], &store, root_tmp.path(), &kb_client)
        .await
        .unwrap();
    assert!(matches!(
        diff.commits_status,
        CommitsStatus::Degraded { .. }
    ));
}

#[tokio::test]
async fn sidechain_and_non_edit_tool_turns_are_excluded_from_the_narrative() {
    let store_tmp = tempfile::tempdir().unwrap();
    // `Arc`-wrapped — `session_diff` now takes `&Arc<Store>` (2026-08-31
    // incident fix: must be able to hand the store off to `run_blocking`'s
    // blocking pool).
    let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
    let root_tmp = tempfile::tempdir().unwrap();
    let root = root_tmp.path();

    let repo_tmp = init_repo();
    let repo_dir = repo_tmp.path();
    let sidechain_path = repo_dir.join("subagent-edit.txt").display().to_string();
    let read_path = repo_dir.join("read-only.txt").display().to_string();

    let lines = vec![
        user_line("u1", "sess-4", &iso(BASE, 0), "prompt"),
        // A subagent (sidechain) edit — excluded from the top-level
        // narrative per the module doc's v1 scope limit.
        tool_use_line(
            "u2",
            "sess-4",
            &iso(BASE, 1_000),
            "Edit",
            &sidechain_path,
            true,
        ),
        // A non-edit tool with a file_path-shaped input — never mutates
        // anything, so it's not uncommitted-edit evidence.
        tool_use_line("u3", "sess-4", &iso(BASE, 2_000), "Read", &read_path, false),
    ];
    index_fixture(&store, root, &lines);

    let kb_client = KbClient::new(daemon_cfg("http://127.0.0.1:0".to_string()));
    let repos = vec![RepoEntry {
        name: "myrepo".to_string(),
        path: repo_dir.to_path_buf(),
    }];
    let diff = session_diff("sess-4", None, &repos, &store, root, &kb_client)
        .await
        .unwrap();

    // Only the prompt segment — neither the sidechain edit nor the Read
    // turn contribute an uncommitted segment.
    assert_eq!(diff.segments.len(), 1, "got {:#?}", as_json(&diff));
    assert!(matches!(diff.segments[0], Segment::Prompt { .. }));
}

#[tokio::test]
async fn repo_filter_excludes_commits_and_edits_outside_it() {
    let store_tmp = tempfile::tempdir().unwrap();
    // `Arc`-wrapped — `session_diff` now takes `&Arc<Store>` (2026-08-31
    // incident fix: must be able to hand the store off to `run_blocking`'s
    // blocking pool).
    let store = Arc::new(Store::open(&store_tmp.path().join("index.db")).unwrap());
    let root_tmp = tempfile::tempdir().unwrap();
    let root = root_tmp.path();

    let repo_a = init_repo();
    let repo_b = init_repo();
    let sha_a = commit_at(repo_a.path(), "a.txt", "x\n", "in repo a", BASE + 1);

    let a_path = repo_a.path().join("a.txt").display().to_string();
    let lines = vec![
        user_line("u1", "sess-5", &iso(BASE, 0), "prompt"),
        tool_use_line("u2", "sess-5", &iso(BASE, 500), "Edit", &a_path, false),
    ];
    index_fixture(&store, root, &lines);

    let (addr, _server) = mock_session_commits(
        "sess-5",
        vec![commit_map_commit_json(
            &sha_a,
            repo_a.path(),
            "in repo a",
            "Ada",
        )],
    )
    .await;
    let kb_client = KbClient::new(daemon_cfg(format!("http://{addr}")));
    let repos = vec![
        RepoEntry {
            name: "repo-a".to_string(),
            path: repo_a.path().to_path_buf(),
        },
        RepoEntry {
            name: "repo-b".to_string(),
            path: repo_b.path().to_path_buf(),
        },
    ];

    // Filtered to repo-b: the commit (in repo-a) and its covered file must
    // both be excluded — a.txt shows up as uncommitted (repo-a isn't in
    // scope to check it against) and no commit segment appears at all.
    let diff = session_diff("sess-5", Some("repo-b"), &repos, &store, root, &kb_client)
        .await
        .unwrap();
    assert_eq!(diff.totals.commits, 0);
    assert!(diff
        .segments
        .iter()
        .all(|s| !matches!(s, Segment::Commits { .. })));
    let has_uncommitted = diff
        .segments
        .iter()
        .any(|s| matches!(s, Segment::Uncommitted { .. }));
    assert!(has_uncommitted, "got {:#?}", as_json(&diff));
}
