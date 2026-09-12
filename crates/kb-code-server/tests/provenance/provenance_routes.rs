//! W3.3 + W3.4 — end-to-end HTTP tests for the provenance surface
//! (`GET /api/why`, `GET /api/story`, `GET /api/provenance-report`),
//! against a real daemon booted via `serve_on_random_port_with_paths` over
//! real git fixture repos. Mirrors `tests/blame_routes.rs`/
//! `tests/join_route.rs`'s own conventions (`boot_with_repos`-style helper,
//! `wait_until_async`, a own small mock-kb-daemon axum server — the
//! `join::kb_client::test_support` helpers are `#[cfg(test)]`-gated inside
//! the library crate and unreachable from this separate integration-test
//! crate, so this file grows its own minimal copy).
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::{git, init_repo};
use axum::extract::Query;
use axum::routing::get;
use axum::{Json, Router};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, TranscriptsSection};
use kb_core::paths::KbPaths;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

// --- git fixture plumbing ---------------------------------------------------

/// One commit with a controlled author date and an arbitrary raw message —
/// mirrors `join::ladder_tests`'s own `commit` helper.
fn commit(dir: &Path, file: &str, contents: &str, message: &str, author_unix: i64) -> String {
    std::fs::write(dir.join(file), contents).unwrap();
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

// --- mock kb daemon (own copy — see module doc) -----------------------------

async fn mock_kb_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

// --- daemon boot -------------------------------------------------------------

async fn wait_until_async<F, Fut>(timeout: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if f().await {
            return true;
        }
        if Instant::now() >= deadline {
            return f().await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(
    repo_name: &str,
    repo_dir: &Path,
    kb_daemon: KbDaemonSection,
    transcripts: Option<TranscriptsSection>,
) -> Boot {
    let mut cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        kb_daemon,
        ..KbCodeConfig::default()
    };
    if let Some(section) = transcripts {
        cfg.transcripts = section;
    }
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
        token_file: None,
        public_url: None,
    }
}

/// Waits for `GET /api/file` to read `path` back successfully — which proves
/// only that the bytes are READABLE FROM DISK, and says **nothing at all
/// about indexing**.
///
/// That route reads the working tree via `fs::read` (`routes::file` →
/// `read_repo_file`, `crates/kb-code-server/src/routes.rs:543-549`),
/// independent of `state.store`; its own doc notes "an unindexed blob just
/// reports empty/`None`". So this wait can return the instant the daemon
/// binds, with the background initial-index walk not yet started. Any test
/// whose asserts read the STORE — `?symbol=` on `/api/story`/`/api/why`
/// resolves through `store::symbols_for_repo` — must additionally wait on
/// [`wait_for_symbols`]; this helper is not a substitute for it.
///
/// `tests/agentview_routes.rs:99-106` makes the same point in the other
/// direction, naming THIS function as the thing it deliberately does not
/// reuse. Keep the two comments in agreement.
async fn wait_for_indexed(base: &str, repo: &str, path: &str) {
    let client = reqwest::Client::new();
    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.to_string();
        let repo = repo.to_string();
        let path = path.to_string();
        async move {
            client
                .get(format!("{base}/api/file"))
                .query(&[("repo", repo.as_str()), ("path", path.as_str())])
                .send()
                .await
                .ok()
                .is_some_and(|r| r.status().is_success())
        }
    })
    .await;
    assert!(ok, "expected {path} to be readable from the working tree");
}

/// Companion wait for tests whose asserts read the `symbols` table (directly
/// or via `?symbol=` on `/api/story`/`/api/why`): polls `GET /api/repos`'
/// `symbol_count` for `repo` until it reaches `expected_symbols`, which only
/// happens once the background index walk has actually run `extract_symbols`
/// + `store::replace_symbols` for the file — the state [`wait_for_indexed`]
///   cannot observe. Same mechanism as `tests/agentview_routes.rs`'s own
///   `wait_for_symbols`; a bounded 15s deadline that fails loudly (never a
///   silent proceed) rather than a bigger blind sleep.
async fn wait_for_symbols(base: &str, repo: &str, expected_symbols: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
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
            Instant::now() < deadline,
            "expected repo {repo:?} to report symbol_count >= {expected_symbols} within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_for_transcripts_indexed(base: &str) {
    let client = reqwest::Client::new();
    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.to_string();
        async move {
            client
                .get(format!("{base}/api/transcripts/status"))
                .send()
                .await
                .ok()
                .and_then(|r| r.error_for_status().ok())
                .is_some()
        }
    })
    .await;
    assert!(ok, "transcripts status route never came up");
    // The status route is up as soon as the daemon boots; poll its OWN
    // `turns` count so we don't race the startup walk.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let body: serde_json::Value = client
            .get(format!("{base}/api/transcripts/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["turns"].as_u64().unwrap_or(0) > 0 || Instant::now() >= deadline {
            assert!(
                body["turns"].as_u64().unwrap_or(0) > 0,
                "turns never indexed: {body}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// --- why: line-grade ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn why_line_on_a_trailer_stamped_commit_resolves_and_enriches_kb_context() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    let sha = commit(
        dir,
        "f.rs",
        "fn a() {}\n",
        "the subject\n\nKb-Session: sess-trailer\n",
        1_700_000_000,
    );

    let matches_body = serde_json::json!({
        "matches": [{
            "kb": "memory",
            "session_id": "sess-trailer",
            "artifact_id": "art-1",
            "kind": "commit",
            "sha": sha,
            "sha_full": sha,
            "resolved": true,
            "subject": "the subject",
            "trailers": ["Kb-Session: sess-trailer"],
            "display_name": "fixed the gizmo race",
            "started_at": 1_700_000_500i64
        }]
    });
    let why_body = serde_json::json!({
        "path": "f.rs",
        "basename": "f.rs",
        "sessions": [{
            "session_id": "sess-trailer",
            "kb": "memory",
            "display_name": "fixed the gizmo race",
            "started_at": 1_700_000_500i64,
            "action": "edit",
            "confidence": "exact",
            "first_user_prompt": "why does the gizmo race?",
            "decisions": [{"kind": "answer", "prompt": "should we lock it?", "answer": "yes"}],
            "commits": []
        }]
    });
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move || {
                let body = matches_body.clone();
                async move { Json(body) }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        )
        .route(
            "/api/why",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let body = why_body.clone();
                async move {
                    assert_eq!(q.get("path").map(String::as_str), Some("f.rs"));
                    Json(body)
                }
            }),
        );
    let (addr, _server) = mock_kb_server(router).await;

    let boot = boot(
        "fixture",
        dir,
        KbDaemonSection {
            enabled: true,
            url: Some(format!("http://{addr}")),
            token_file: None,
            public_url: None,
        },
        None,
    )
    .await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/why", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("line", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["line"], 1);
    assert_eq!(body["attribution"]["confidence"], "trailer");
    assert_eq!(body["attribution"]["via"], "commit-trailer");
    assert_eq!(body["attribution"]["session_id"], "sess-trailer");
    assert_eq!(body["attribution"]["display_name"], "fixed the gizmo race");
    // The ladder's own `by-commit` enrichment already resolved `kb` (the
    // mock `matches_body` above carries it) — unconditional, not sourced
    // from the loopback-gated follow-up.
    assert_eq!(body["attribution"]["kb"], "memory");
    assert_eq!(body["timeline_available"], true);
    assert_eq!(
        body["kb_context"]["prompt_excerpt"],
        "why does the gizmo race?"
    );
    let decisions = body["kb_context"]["decisions"].as_array().unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["kind"], "answer");
    // No `/api/sessions/{id}` route is mocked on this router, so the
    // `session_detail` follow-up 404s (Ok(None)) -> empty `memory_ids` ->
    // OMITTED from the wire, never `[]`.
    assert!(
        body.as_object()
            .unwrap()
            .get("session_memory_ids")
            .is_none(),
        "body: {body}"
    );
}

/// CT-A2 — the SAME loopback-gated follow-up that resolves `kb_context`
/// ALSO calls `GET /api/sessions/{id}` for `memory_ids`; when kb has one,
/// it rides the wire as `session_memory_ids` (display-only ids).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn why_line_on_a_trailer_stamped_commit_surfaces_session_memory_ids() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    let sha = commit(
        dir,
        "f.rs",
        "fn a() {}\n",
        "the subject\n\nKb-Session: sess-trailer\n",
        1_700_000_000,
    );

    let matches_body = serde_json::json!({
        "matches": [{
            "kb": "memory",
            "session_id": "sess-trailer",
            "artifact_id": "art-1",
            "kind": "commit",
            "sha": sha,
            "sha_full": sha,
            "resolved": true,
            "subject": "the subject",
            "trailers": ["Kb-Session: sess-trailer"],
            "display_name": "fixed the gizmo race",
            "started_at": 1_700_000_500i64
        }]
    });
    let why_body = serde_json::json!({
        "path": "f.rs",
        "basename": "f.rs",
        "sessions": [{
            "session_id": "sess-trailer",
            "kb": "memory",
            "display_name": "fixed the gizmo race",
            "started_at": 1_700_000_500i64,
            "action": "edit",
            "confidence": "exact",
            "decisions": [],
            "commits": []
        }]
    });
    let session_detail_body = serde_json::json!({
        "id": "art-1",
        "kb": "memory",
        "artifact_id": "art-1",
        "session_id": "sess-trailer",
        "started_at": 1_700_000_500i64,
        "ended_at": 1_700_000_600i64,
        "duration_ms": 100_000,
        "message_count": 3,
        "memory_count": 2,
        "source_relative": "sessions/sess-trailer.html",
        "display_name": "fixed the gizmo race",
        "files_read_count": 1,
        "files_edited_count": 1,
        "memory_ids": ["mem-a", "mem-b"]
    });
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move || {
                let body = matches_body.clone();
                async move { Json(body) }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        )
        .route(
            "/api/why",
            get(move || {
                let body = why_body.clone();
                async move { Json(body) }
            }),
        )
        .route(
            "/api/sessions/{session_id}",
            get(
                move |axum::extract::Path(session_id): axum::extract::Path<String>| {
                    let body = session_detail_body.clone();
                    async move {
                        assert_eq!(session_id, "sess-trailer");
                        Json(body)
                    }
                },
            ),
        );
    let (addr, _server) = mock_kb_server(router).await;

    let boot = boot(
        "fixture",
        dir,
        KbDaemonSection {
            enabled: true,
            url: Some(format!("http://{addr}")),
            token_file: None,
            public_url: None,
        },
        None,
    )
    .await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/why", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("line", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["attribution"]["kb"], "memory");
    assert_eq!(
        body["session_memory_ids"],
        serde_json::json!(["mem-a", "mem-b"])
    );
}

// Finding 2 (security review) — `kb_context` (transcript-derived prompt +
// decision text) must be loopback-only; a non-loopback caller must still
// get full attribution (session id, confidence, `via`), but never
// `kb_context` — see `provenance::why`'s module doc ("Sensitivity").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn why_line_non_loopback_caller_gets_attribution_but_never_kb_context() {
    // A bearer token so the spoofed non-loopback request below actually
    // reaches `provenance::why::why` (`auth_bearer` 401s a token-less
    // non-loopback caller, invariant #4) — same technique
    // `tests/search_unified.rs`'s `non_loopback_caller_never_sees_a_
    // transcripts_section` uses.
    const FIXTURE_TOKEN: &str = "kb-code-why-non-loopback-test-token";
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    let sha = commit(
        dir,
        "f.rs",
        "fn a() {}\n",
        "the subject\n\nKb-Session: sess-trailer\n",
        1_700_000_000,
    );

    let matches_body = serde_json::json!({
        "matches": [{
            "kb": "memory",
            "session_id": "sess-trailer",
            "artifact_id": "art-1",
            "kind": "commit",
            "sha": sha,
            "sha_full": sha,
            "resolved": true,
            "subject": "the subject",
            "trailers": ["Kb-Session: sess-trailer"],
            "display_name": "fixed the gizmo race",
            "started_at": 1_700_000_500i64
        }]
    });
    // If the daemon ever called this (it must NOT, for a non-loopback
    // caller), the test would see a `kb_context` in the response and fail
    // the assertions below — this mock exists purely so a regression would
    // be OBSERVABLE (a silently-skipped call and a silently-unreachable kb
    // daemon would otherwise look identical from the response alone).
    let why_body = serde_json::json!({
        "path": "f.rs",
        "basename": "f.rs",
        "sessions": [{
            "session_id": "sess-trailer",
            "kb": "memory",
            "display_name": "fixed the gizmo race",
            "started_at": 1_700_000_500i64,
            "action": "edit",
            "confidence": "exact",
            "first_user_prompt": "why does the gizmo race?",
            "decisions": [{"kind": "answer", "prompt": "should we lock it?", "answer": "yes"}],
            "commits": []
        }]
    });
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move || {
                let body = matches_body.clone();
                async move { Json(body) }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        )
        .route(
            "/api/why",
            get(move || {
                let body = why_body.clone();
                async move { Json(body) }
            }),
        );
    let (addr, _server) = mock_kb_server(router).await;

    std::env::remove_var("KB_ALLOW_NO_AUTH");
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);
    let boot_result = boot(
        "fixture",
        dir,
        KbDaemonSection {
            enabled: true,
            url: Some(format!("http://{addr}")),
            token_file: None,
            public_url: None,
        },
        None,
    )
    .await;
    std::env::remove_var("KB_CODE_TOKEN");
    wait_for_indexed(&boot_result.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/why", boot_result.base))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("line", "1")])
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    // Attribution resolves fully either way — only `kb_context` is gated.
    assert_eq!(body["attribution"]["confidence"], "trailer");
    assert_eq!(body["attribution"]["via"], "commit-trailer");
    assert_eq!(body["attribution"]["session_id"], "sess-trailer");
    assert_eq!(body["attribution"]["display_name"], "fixed the gizmo race");
    // `attribution.kb` is sourced from the LADDER's own `by-commit`
    // enrichment (unconditional), NOT the loopback-gated `kb_context`
    // follow-up — a non-loopback caller still gets it.
    assert_eq!(body["attribution"]["kb"], "memory");
    // `kb_context` must be ABSENT (never even `null`) — the follow-up to kb
    // must never have been made for a non-loopback caller.
    assert!(
        body.as_object().unwrap().get("kb_context").is_none(),
        "body: {body}"
    );
    // `session_memory_ids` rides the SAME loopback-gated follow-up as
    // `kb_context` (a second call the follow-up makes) — also absent.
    assert!(
        body.as_object()
            .unwrap()
            .get("session_memory_ids")
            .is_none(),
        "body: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn why_line_on_a_plain_commit_with_kb_unreachable_resolves_none() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "f.rs",
        "fn a() {}\n",
        "an unremarkable commit",
        1_700_000_000,
    );

    let boot = boot("fixture", dir, disabled_kb_daemon(), None).await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/why", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("line", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["attribution"]["confidence"], "none");
    assert_eq!(body["attribution"]["via"], "kb-disabled");
    assert!(body["attribution"]["session_id"].is_null());
    assert!(body["kb_context"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn why_line_on_an_uncommitted_edit_answers_from_the_transcripts_index() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "f.rs", "fn a() {}\n", "c1", 1_700_000_000);

    // Dirty the file with an uncommitted second line.
    std::fs::write(dir.join("f.rs"), "fn a() {}\nfn b_uncommitted() {}\n").unwrap();
    let abs_path = std::fs::canonicalize(dir).unwrap().join("f.rs");

    // Plant a matching transcript turn (a `tool_use` Edit call whose
    // `file_path` is this exact absolute path) for an in-flight session.
    let transcripts_tmp = tempfile::tempdir().unwrap();
    let transcripts_root = transcripts_tmp.path().join("transcripts");
    let proj = transcripts_root.join("-fixture-project");
    std::fs::create_dir_all(&proj).unwrap();
    let abs_path_json = serde_json::to_string(&abs_path.to_string_lossy().into_owned()).unwrap();
    let line = format!(
        r#"{{"type":"assistant","uuid":"u1","parentUuid":null,"sessionId":"sess-live","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Edit","input":{{"file_path":{abs_path_json},"old_string":"a","new_string":"b"}}}}]}}}}"#
    );
    std::fs::write(proj.join("s1.jsonl"), format!("{line}\n")).unwrap();

    let boot = boot(
        "fixture",
        dir,
        disabled_kb_daemon(),
        Some(TranscriptsSection {
            enabled: true,
            root: transcripts_root.to_string_lossy().into_owned(),
            exclude_projects: Vec::new(),
            index_thinking: true,
        }),
    )
    .await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;
    wait_for_transcripts_indexed(&boot.base).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/why", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("line", "2")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["region"]["sha"], "0".repeat(40));
    assert_eq!(body["attribution"]["confidence"], "none");
    assert_eq!(body["attribution"]["via"], "uncommitted-live");
    assert!(body["attribution"]["session_id"].is_null());
    let session_ids = body["attribution"]["session_ids"].as_array().unwrap();
    assert_eq!(session_ids, &vec![serde_json::json!("sess-live")]);
    assert_eq!(body["timeline_available"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn why_line_uncommitted_with_no_matching_transcript_is_an_honest_gap() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "f.rs", "fn a() {}\n", "c1", 1_700_000_000);
    std::fs::write(dir.join("f.rs"), "fn a() {}\nfn b_uncommitted() {}\n").unwrap();

    let boot = boot("fixture", dir, disabled_kb_daemon(), None).await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/why", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("line", "2")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["attribution"]["via"], "uncommitted-live");
    let session_ids = body["attribution"]["session_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        session_ids.is_empty(),
        "no fixture transcript planted: {body}"
    );
}

// --- why: file-grade ----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn why_file_grade_ranks_the_dominant_session_first() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    // Alice's trailer-stamped commit writes 4 of 5 lines; Bob's plain
    // (untrailered) commit then edits just ONE of them.
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
        "alice writes the file\n\nKb-Session: sess-alice\n",
        1_700_000_000,
    );
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b_edited() {}\nfn c() {}\nfn d() {}\n",
        "bob edits one line",
        1_700_000_100,
    );

    let boot = boot("fixture", dir, disabled_kb_daemon(), None).await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/why", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let sessions = body["sessions"].as_array().unwrap();
    assert!(sessions.len() >= 2, "body: {body}");
    assert_eq!(sessions[0]["session_id"], "sess-alice");
    assert_eq!(sessions[0]["confidence"], "trailer");
    assert_eq!(sessions[0]["lines"], 3);
    // Bob's commit resolved no session (kb disabled) — grouped by its own
    // sha, reported second (fewer lines).
    assert!(sessions[1]["session_id"].is_null());
    assert_eq!(sessions[1]["confidence"], "none");
    assert_eq!(sessions[1]["lines"], 1);
    assert_eq!(body["uncommitted_lines"], 0);
}

// --- story ---------------------------------------------------------------------

/// Every commit carries its own `Kb-Session:` trailer (arm 1 — pure local,
/// resolves fine with the kb daemon disabled), so all three are COVERED and
/// the CT-E2 gap collapse is the identity: the pre-gap grouping/ordering
/// contract must hold unchanged, with no `"gap"` beat anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn story_groups_and_orders_chronologically_with_owns_vs_historical() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b() {}\nfn c() {}\n",
        "c1: alice adds three fns\n\nKb-Session: sess-alice\n",
        1_700_000_000,
    );
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b_edited() {}\nfn c() {}\n",
        "c2: bob edits fn b\n\nKb-Session: sess-bob\n",
        1_700_000_100,
    );
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b_edited_again() {}\nfn c() {}\n",
        "c3: carol edits fn b again\n\nKb-Session: sess-carol\n",
        1_700_000_200,
    );

    let boot = boot("fixture", dir, disabled_kb_daemon(), None).await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/story", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3, "body: {body}");

    // Chronological: alice (earliest, owns lines 1+3), bob (historical —
    // superseded on line 2), carol (owns line 2 currently).
    assert_eq!(entries[0]["subject"], "c1: alice adds three fns");
    assert_eq!(entries[0]["session_id"], "sess-alice");
    assert_eq!(entries[0]["status"], "owns-lines");
    assert_eq!(entries[0]["lines_touched"], 2);

    assert_eq!(entries[1]["subject"], "c2: bob edits fn b");
    assert_eq!(entries[1]["session_id"], "sess-bob");
    assert_eq!(entries[1]["status"], "historical");

    assert_eq!(entries[2]["subject"], "c3: carol edits fn b again");
    assert_eq!(entries[2]["session_id"], "sess-carol");
    assert_eq!(entries[2]["status"], "owns-lines");
    assert_eq!(entries[2]["lines_touched"], 1);

    // Fully covered ⇒ no gap beat and no gap-only field on any entry.
    for e in entries {
        assert_ne!(e["status"], "gap", "body: {body}");
        assert!(e.get("commit_count").is_none(), "body: {body}");
        assert!(e.get("reason").is_none(), "body: {body}");
        assert!(e.get("last_seen").is_none(), "body: {body}");
    }
}

/// The same three commits WITHOUT trailers and with the kb daemon disabled:
/// every resolution is session-less (`via: "kb-disabled"`), so the whole
/// story collapses into ONE attention-gap beat whose reason is the honest
/// weaker claim — "join unavailable", NOT "no captured session" (kb was
/// never consulted, coverage is unknown).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn story_collapses_a_sessionless_run_into_one_gap_beat() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b() {}\nfn c() {}\n",
        "c1: alice adds three fns",
        1_700_000_000,
    );
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b_edited() {}\nfn c() {}\n",
        "c2: bob edits fn b",
        1_700_000_100,
    );
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b_edited_again() {}\nfn c() {}\n",
        "c3: carol edits fn b again",
        1_700_000_200,
    );

    let boot = boot("fixture", dir, disabled_kb_daemon(), None).await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/story", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let entries = body["entries"].as_array().unwrap();
    assert_eq!(
        entries.len(),
        1,
        "one gap beat, never one per commit: {body}"
    );
    let gap = &entries[0];
    assert_eq!(gap["status"], "gap");
    assert_eq!(gap["commit_count"], 3);
    assert_eq!(gap["first_seen"], 1_700_000_000i64);
    assert_eq!(gap["last_seen"], 1_700_000_200i64);
    assert_eq!(gap["reason"], "join-unavailable");
    assert_eq!(gap["via"], "kb-disabled");
    assert_eq!(gap["confidence"], "none");
    // alice owns 2 current lines, carol 1, bob 1 bounded-timeline touch.
    assert_eq!(gap["lines_touched"], 4, "body: {body}");
    // A multi-commit gap speaks for the run — no single identity.
    assert!(gap.get("session_id").is_none(), "body: {body}");
    assert!(gap.get("sha").is_none(), "body: {body}");
    assert!(gap.get("subject").is_none(), "body: {body}");
}

/// Mixed coverage against a REACHABLE (but empty) mock kb: the covered
/// trailer commit stays a full beat, the two session-less ones behind it
/// collapse into one gap beat ordered after it — and the reason upgrades to
/// the definite `"no-captured-session"` (kb answered; nothing matched).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn story_mixed_coverage_interleaves_gap_beats_with_covered_ones() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b() {}\nfn c() {}\n",
        "c1: alice adds three fns\n\nKb-Session: sess-alice\n",
        1_700_000_000,
    );
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b_edited() {}\nfn c() {}\n",
        "c2: bob edits fn b",
        1_700_000_100,
    );
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b_edited_again() {}\nfn c() {}\n",
        "c3: carol edits fn b again",
        1_700_000_200,
    );

    // Reachable mock that knows nothing: every by-commit lookup returns an
    // empty match list, the commit-map snapshot is empty — arms 2-5 all run
    // and miss, so a trailer-less commit lands at `via: "no-match"`.
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(|| async { Json(serde_json::json!({ "matches": [] })) }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        );
    let (addr, _server) = mock_kb_server(router).await;

    let boot = boot(
        "fixture",
        dir,
        KbDaemonSection {
            enabled: true,
            url: Some(format!("http://{addr}")),
            token_file: None,
            public_url: None,
        },
        None,
    )
    .await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/story", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "covered beat + ONE gap beat: {body}");

    assert_eq!(entries[0]["session_id"], "sess-alice");
    assert_eq!(entries[0]["subject"], "c1: alice adds three fns");
    assert_eq!(entries[0]["status"], "owns-lines");
    assert!(entries[0].get("commit_count").is_none(), "body: {body}");

    let gap = &entries[1];
    assert_eq!(gap["status"], "gap");
    assert_eq!(gap["commit_count"], 2);
    assert_eq!(gap["first_seen"], 1_700_000_100i64);
    assert_eq!(gap["last_seen"], 1_700_000_200i64);
    assert_eq!(gap["reason"], "no-captured-session");
    assert_eq!(gap["via"], "no-match");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn story_symbol_filter_restricts_to_the_symbol_line_range() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    // Both functions keep their NAME stable across every commit (only the
    // body content changes) — `?symbol=b` resolves against HEAD's current
    // symbol table by name, so a renamed function would never be found.
    // Every commit carries a trailer (covered) so the CT-E2 gap collapse
    // stays the identity and each subject remains individually assertable.
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b() {}\n",
        "c1: alice adds a and b\n\nKb-Session: sess-alice\n",
        1_700_000_000,
    );
    commit(
        dir,
        "f.rs",
        "fn a() {}\nfn b() { 1; }\n",
        "c2: bob edits b\n\nKb-Session: sess-bob\n",
        1_700_000_100,
    );
    // A THIRD commit that only touches `a` — must be excluded from the
    // `?symbol=b` story entirely.
    commit(
        dir,
        "f.rs",
        "fn a() { 2; }\nfn b() { 1; }\n",
        "c3: carol edits a\n\nKb-Session: sess-carol\n",
        1_700_000_200,
    );

    let boot = boot("fixture", dir, disabled_kb_daemon(), None).await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;
    // `?symbol=b` resolves via `story::resolve_symbol_range` ->
    // `store::symbols_for_repo` — `wait_for_indexed`'s `/api/file` readiness
    // check says nothing about whether the SYMBOL TABLE has landed yet (see
    // that fn's doc). HEAD's f.rs content is `fn a() {...}\nfn b() {...}\n`
    // — 2 symbols.
    wait_for_symbols(&boot.base, "fixture", 2).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/story", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("symbol", "b")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["symbol"], "b");
    let entries = body["entries"].as_array().unwrap();
    let subjects: Vec<&str> = entries
        .iter()
        .map(|e| e["subject"].as_str().unwrap())
        .collect();
    assert!(
        subjects.iter().any(|s| s.contains("alice adds a and b")),
        "b's origin must appear (historical): {subjects:?}"
    );
    assert!(
        subjects.iter().any(|s| s.contains("bob edits b")),
        "b's current owner must appear (owns-lines): {subjects:?}"
    );
    assert!(
        !subjects.iter().any(|s| s.contains("carol edits a")),
        "carol only touched `a`, must not appear in the `b` story: {subjects:?}"
    );

    let unknown = client
        .get(format!("{}/api/story", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "f.rs"),
            ("symbol", "does_not_exist"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
}

// --- provenance-report ----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provenance_report_counts_match_a_hand_built_fixture_history() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);

    let week1 = 1_750_000_000i64; // 2025-06-15T16:26:40Z
    let week2 = week1 + 7 * 24 * 60 * 60; // exactly one ISO week later

    let sha_plain1 = commit(dir, "a.txt", "v1\n", "week1 plain commit", week1);
    let sha_trailer1 = commit(
        dir,
        "a.txt",
        "v2\n",
        "week1 trailer commit\n\nKb-Session: sess-w1\n",
        week1 + 60,
    );
    let sha_plain2 = commit(dir, "a.txt", "v3\n", "week2 plain commit", week2);
    let sha_trailer2 = commit(
        dir,
        "a.txt",
        "v4\n",
        "week2 trailer commit\n\nKb-Session: sess-w2\n",
        week2 + 60,
    );

    // Reachable mock: matches only the two trailer shas (both `started_at`
    // WELL before week1, so `capture_era` covers the whole 4-commit walk);
    // every other sha (including the plain commits' own arm-2 lookup)
    // gets an empty match list.
    let capture_era_start = week1 - 10_000;
    let by_commit_map: HashMap<String, serde_json::Value> = HashMap::from([
        (
            sha_trailer1.clone(),
            serde_json::json!({ "matches": [{
                "kb": "memory", "session_id": "sess-w1", "artifact_id": "a1", "kind": "commit",
                "sha": sha_trailer1, "sha_full": sha_trailer1, "resolved": true,
                "subject": "week1 trailer commit", "trailers": ["Kb-Session: sess-w1"],
                "display_name": "week1 session", "started_at": capture_era_start
            }]}),
        ),
        (
            sha_trailer2.clone(),
            serde_json::json!({ "matches": [{
                "kb": "memory", "session_id": "sess-w2", "artifact_id": "a2", "kind": "commit",
                "sha": sha_trailer2, "sha_full": sha_trailer2, "resolved": true,
                "subject": "week2 trailer commit", "trailers": ["Kb-Session: sess-w2"],
                "display_name": "week2 session", "started_at": capture_era_start
            }]}),
        ),
    ]);
    let by_commit_map = std::sync::Arc::new(by_commit_map);
    let router = Router::new()
        .route(
            "/api/sessions/by-commit",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let by_commit_map = by_commit_map.clone();
                async move {
                    let sha = q.get("sha").cloned().unwrap_or_default();
                    match by_commit_map.get(&sha) {
                        Some(body) => Json(body.clone()),
                        None => Json(serde_json::json!({ "matches": [] })),
                    }
                }
            }),
        )
        .route(
            "/api/sessions/commit-map",
            get(|| async { Json(serde_json::json!({ "commits": [], "limit": 500, "offset": 0 })) }),
        );
    let (addr, _server) = mock_kb_server(router).await;

    let boot = boot(
        "fixture",
        dir,
        KbDaemonSection {
            enabled: true,
            url: Some(format!("http://{addr}")),
            token_file: None,
            public_url: None,
        },
        None,
    )
    .await;
    wait_for_indexed(&boot.base, "fixture", "a.txt").await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/provenance-report", boot.base))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["total_commits"], 4, "body: {body}");
    assert_eq!(body["truncated"], false);

    let by_conf: HashMap<&str, (u64, f64)> = body["by_confidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            (
                b["label"].as_str().unwrap(),
                (b["count"].as_u64().unwrap(), b["pct"].as_f64().unwrap()),
            )
        })
        .collect();
    assert_eq!(by_conf["trailer"].0, 2);
    assert_eq!(by_conf["exact"].0, 0);
    assert_eq!(by_conf["fuzzy"].0, 0);
    assert_eq!(by_conf["none"].0, 2);
    assert!((by_conf["trailer"].1 - 50.0).abs() < 1e-6);

    let by_via: HashMap<&str, u64> = body["by_via"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["label"].as_str().unwrap(), b["count"].as_u64().unwrap()))
        .collect();
    assert_eq!(by_via["commit-trailer"], 2);
    assert_eq!(by_via["no-match"], 2);

    let weeks = body["trailer_coverage_by_week"].as_array().unwrap();
    assert_eq!(
        weeks.len(),
        2,
        "the two commit pairs land 7 days apart — must be TWO distinct ISO weeks: {weeks:?}"
    );
    for w in weeks {
        assert_eq!(w["commits"], 2);
        assert_eq!(w["trailer_commits"], 1);
        assert_eq!(w["pct"], 50.0);
    }
    // Newest-first commit walk groups into ascending-sorted week labels
    // (BTreeMap) — the earlier week's label must sort before the later one.
    assert!(weeks[0]["week"].as_str().unwrap() < weeks[1]["week"].as_str().unwrap());

    let era = &body["capture_era"];
    assert!(!era.is_null(), "body: {body}");
    assert_eq!(era["start"], capture_era_start);
    assert_eq!(
        era["commit_count"], 4,
        "the era start predates every commit"
    );
    let era_by_conf: HashMap<&str, u64> = era["by_confidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["label"].as_str().unwrap(), b["count"].as_u64().unwrap()))
        .collect();
    assert_eq!(era_by_conf["trailer"], 2);
    assert_eq!(era_by_conf["none"], 2);

    // sha_plain1/sha_plain2 exist purely so `commit()` compiles with unique
    // content per step; silence the unused-var lint if the assertions above
    // are ever trimmed.
    let _ = (&sha_plain1, &sha_plain2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provenance_report_unknown_repo_404s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.txt", "v1\n", "c1", 1_700_000_000);

    let boot = boot("fixture", dir, disabled_kb_daemon(), None).await;
    wait_for_indexed(&boot.base, "fixture", "a.txt").await;

    let client = reqwest::Client::new();
    let status = client
        .get(format!("{}/api/provenance-report", boot.base))
        .query(&[("repo", "no-such-repo")])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);
}
