//! Phase E3 — end-to-end HTTP tests for `GET`/`POST /api/sets`,
//! `GET`/`PATCH`/`DELETE /api/sets/{id}`, `POST /api/sets/{id}/spans`,
//! `POST /api/sets/from-session`, `POST /api/sets/from-doc` (DCB-W3.C.R —
//! see the "from-doc" section near the bottom), and the `GET /api/pack?set=`
//! integration, against a real daemon booted via
//! `serve_on_random_port_with_paths`. Mirrors `tests/annotations_route.rs`'s
//! own conventions (`boot_with_repo`, `SERIAL` guard, fixture repo) and
//! `tests/agentview_routes.rs`'s (`wait_for_indexed`, for the pack
//! integration).
//!
//! The from-doc tests run their OWN minimal mock kb daemon
//! (`mock_kb_from_doc`) rather than reusing `doclens_route.rs`'s elaborate
//! golden-table fixture (`piano-fixture.json` + the full path/line-state
//! matrix) — that harness answers a different question (line-state
//! resolution correctness across every `path_state × line_state`
//! combination); this route's own tests only need ONE resolvable doc to
//! exercise create/404/409 wiring.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

mod common;

use axum::{extract::Path as AxumPath, response::IntoResponse, routing::get, Router};
use kb_code_server::config::{
    DoclensSection, KbCodeConfig, KbDaemonSection, RepoEntry, TranscriptsSection,
};
use kb_core::paths::KbPaths;
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: "http://127.0.0.1:0".to_string(),
        token_file: None,
        public_url: None,
    }
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(repos: Vec<RepoEntry>, transcripts: Option<TranscriptsSection>) -> Boot {
    let cfg = KbCodeConfig {
        repos,
        kb_daemon: disabled_kb_daemon(),
        transcripts: transcripts.unwrap_or_default(),
        ..KbCodeConfig::default()
    };
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

async fn boot_with_repo(name: &str, path: &Path) -> Boot {
    boot(
        vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        None,
    )
    .await
}

/// [`boot`], but with `[kb_daemon]` pointed at `kb_addr` (a mock — see
/// [`mock_kb_from_doc`]) and `[doclens] kbs = ["platform"]`, the minimum
/// config `POST /api/sets/from-doc`'s own `resolve_lens` call needs to get
/// past its gate (`doclens::gate_kb_only` 403s an empty allowlist).
async fn boot_from_doc(repo_name: &str, repo_path: &Path, kb_addr: SocketAddr) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: true,
            url: format!("http://{kb_addr}"),
            token_file: None,
            public_url: None,
        },
        doclens: DoclensSection {
            kbs: vec!["platform".to_string()],
            ..DoclensSection::default()
        },
        ..KbCodeConfig::default()
    };
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

/// Minimal mock kb daemon for `POST /api/sets/from-doc`'s own route tests
/// (below) — a single `coderef/1` doc (`"doc1"`) with ONE `path`-kind ref
/// (`lib.rs`, no line hint — matches `fixture_repo`'s own `lib.rs`), enough
/// to exercise materialize-into-a-set plus the 404 (unknown doc)/409
/// (explicit-name collision) wiring. Any other doc id 404s.
async fn mock_kb_from_doc() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let router = Router::new().route(
        "/api/kb/{kb}/docs/{id}/code-refs",
        get(
            |AxumPath((kb, id)): AxumPath<(String, String)>| async move {
                if id == "doc1" {
                    axum::Json(serde_json::json!({
                        "schema": "coderef/1",
                        "kb": kb,
                        "doc_id": "doc1",
                        "doc_path": "docs/x.html",
                        "title": "From-doc route test",
                        "doc_hash": "hash1",
                        "extracted_at": 1,
                        "never_scanned": false,
                        "code_rev": null,
                        "ref_count": 1,
                        "ungrouped_count": 1,
                        "truncated": false,
                        "groups": [],
                        "refs": [{
                            "ordinal": 0,
                            "group": null,
                            "kind": "path",
                            "raw": "lib.rs",
                            "path_hint": "lib.rs",
                            "line_start": null,
                            "line_end": null,
                            "line_spans": null,
                            "symbol_container": null,
                            "symbol_member": null,
                            "context": null,
                            "context_tokens": [],
                            "declared": false
                        }]
                    }))
                    .into_response()
                } else {
                    (
                        axum::http::StatusCode::NOT_FOUND,
                        axum::Json(serde_json::json!({"error": "no such doc"})),
                    )
                        .into_response()
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (addr, handle)
}

/// Mirrors `tests/agentview_routes.rs`'s own wait — `GET /api/pack?set=`
/// reads the store's `symbols`/`files` tables, so tests that exercise it
/// must wait for the boot walk first.
async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
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
            Instant::now() < deadline,
            "expected repo {repo:?} to report file_count >= {expected_files} within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn span(path: &str) -> serde_json::Value {
    serde_json::json!({ "path": path })
}

fn ranged_span(path: &str, start: u32, end: u32, note: &str) -> serde_json::Value {
    serde_json::json!({ "path": path, "line_start": start, "line_end": end, "note": note })
}

// =====================================================================
// CRUD
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_list_get_round_trip() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let create = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({
            "repo": "r",
            "name": "the ingest path",
            "description": "how a request flows in",
            "spans": [span("lib.rs"), ranged_span("main.rs", 1, 1, "entrypoint")],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = create.json().await.unwrap();
    assert_eq!(created["schema"], "sets/1");
    assert_eq!(created["name"], "the ingest path");
    assert_eq!(created["repo"], "r");
    let id = created["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("set_"));
    assert_eq!(created["spans"].as_array().unwrap().len(), 2);
    assert_eq!(created["spans"][0]["ordinal"], 0);
    assert_eq!(created["spans"][0]["path"], "lib.rs");
    assert_eq!(created["spans"][1]["line_start"], 1);
    assert_eq!(created["spans"][1]["note"], "entrypoint");

    let list: serde_json::Value = client
        .get(format!("{}/api/sets", boot.base))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sets = list["sets"].as_array().unwrap();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0]["id"], id);
    assert_eq!(sets[0]["span_count"], 2);

    let got: serde_json::Value = client
        .get(format!("{}/api/sets/{id}", boot.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["id"], id);
    assert_eq!(got["spans"].as_array().unwrap().len(), 2);

    let missing = client
        .get(format!("{}/api/sets/set_does_not_exist", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_rejects_a_duplicate_name_in_the_same_repo() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let body = serde_json::json!({ "repo": "r", "name": "dup", "spans": [] });
    let first = client
        .post(format!("{}/api/sets", boot.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);

    let second = client
        .post(format!("{}/api/sets", boot.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_validates_spans() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let escaping = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({
            "repo": "r", "name": "bad1", "spans": [span("../etc/passwd")],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(escaping.status(), reqwest::StatusCode::BAD_REQUEST);

    let backwards = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({
            "repo": "r", "name": "bad2",
            "spans": [ranged_span("lib.rs", 9, 2, "n")],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(backwards.status(), reqwest::StatusCode::BAD_REQUEST);

    let half_range = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({
            "repo": "r", "name": "bad3",
            "spans": [{"path": "lib.rs", "line_start": 2}],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(half_range.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_replaces_spans_and_updates_meta_in_one_call() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({
            "repo": "r", "name": "one", "spans": [span("lib.rs"), span("main.rs")],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let patched = client
        .patch(format!("{}/api/sets/{id}", boot.base))
        .json(&serde_json::json!({
            "name": "renamed",
            "description": "new desc",
            "spans": [span("main.rs")],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(patched.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = patched.json().await.unwrap();
    assert_eq!(body["name"], "renamed");
    assert_eq!(body["description"], "new desc");
    let spans = body["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1, "full replacement, not appended to");
    assert_eq!(spans[0]["path"], "main.rs");
    assert_eq!(spans[0]["ordinal"], 0);

    let missing = client
        .patch(format!("{}/api/sets/set_nope", boot.base))
        .json(&serde_json::json!({ "name": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_rejects_a_rename_that_collides_with_a_different_set() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({ "repo": "r", "name": "one", "spans": [] }))
        .send()
        .await
        .unwrap();
    let two: serde_json::Value = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({ "repo": "r", "name": "two", "spans": [] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = two["id"].as_str().unwrap();

    let resp = client
        .patch(format!("{}/api/sets/{id}", boot.base))
        .json(&serde_json::json!({ "name": "one" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_span_adds_after_the_last_ordinal() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({ "repo": "r", "name": "s", "spans": [span("lib.rs")] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let appended = client
        .post(format!("{}/api/sets/{id}/spans", boot.base))
        .json(&ranged_span("main.rs", 1, 1, "entry"))
        .send()
        .await
        .unwrap();
    assert_eq!(appended.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = appended.json().await.unwrap();
    let spans = body["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[1]["ordinal"], 1);
    assert_eq!(spans[1]["path"], "main.rs");

    let missing = client
        .post(format!("{}/api/sets/set_nope/spans", boot.base))
        .json(&span("x.rs"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_cascades_and_404s_afterward() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({ "repo": "r", "name": "s", "spans": [span("lib.rs")] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let deleted = client
        .delete(format!("{}/api/sets/{id}", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    let get_after = client
        .get(format!("{}/api/sets/{id}", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(get_after.status(), reqwest::StatusCode::NOT_FOUND);

    let delete_again = client
        .delete(format!("{}/api/sets/{id}", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(delete_again.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_changed_event_fires_on_the_sse_bus() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let sse_resp = client
        .get(format!("{}/api/events", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), reqwest::StatusCode::OK);
    let mut stream = sse_resp.bytes_stream();

    let create = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({ "repo": "r", "name": "s", "spans": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), reqwest::StatusCode::CREATED);

    use futures::StreamExt;
    let found = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut buf = String::new();
        loop {
            let chunk = stream.next().await.expect("stream item").unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if buf.contains("set.changed") {
                return true;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(found, "expected a set.changed SSE frame");
}

// =====================================================================
// GET /api/pack?set= integration
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_route_resolves_a_set_and_annotates_entries() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("fixture", repo_tmp.path()).await;
    wait_for_indexed(&boot.base, "fixture", 2).await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({
            "repo": "fixture",
            "name": "s",
            "spans": [ranged_span("lib.rs", 1, 1, "look here"), span("main.rs")],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let resp = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "fixture"), ("set", id)])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "pack/1", "additive — schema unchanged");
    assert_eq!(body["set_id"], id);
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["path"], "lib.rs");
    assert_eq!(files[0]["lines"]["start"], 1);
    assert_eq!(files[0]["lines"]["end"], 1);
    assert_eq!(files[0]["span_note"], "look here");
    assert_eq!(files[1]["path"], "main.rs");
    assert!(
        files[1]["lines"].is_null(),
        "whole-file span carries no lines"
    );
    assert!(files[1]["span_note"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_route_rejects_both_paths_and_set_and_neither() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("fixture", repo_tmp.path()).await;
    wait_for_indexed(&boot.base, "fixture", 2).await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({ "repo": "fixture", "name": "s", "spans": [span("lib.rs")] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap();

    let both = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "fixture"), ("paths", "lib.rs"), ("set", id)])
        .send()
        .await
        .unwrap();
    assert_eq!(both.status(), reqwest::StatusCode::BAD_REQUEST);

    let neither = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap();
    assert_eq!(neither.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_route_404s_an_unknown_or_cross_repo_set() {
    let repo_a = fixture_repo();
    let repo_b = fixture_repo();
    let boot = boot(
        vec![
            RepoEntry {
                name: "a".to_string(),
                path: std::fs::canonicalize(repo_a.path()).unwrap(),
            },
            RepoEntry {
                name: "b".to_string(),
                path: std::fs::canonicalize(repo_b.path()).unwrap(),
            },
        ],
        None,
    )
    .await;
    wait_for_indexed(&boot.base, "a", 2).await;
    wait_for_indexed(&boot.base, "b", 2).await;
    let client = reqwest::Client::new();

    let unknown = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "a"), ("set", "set_does_not_exist")])
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);

    let created_in_a: serde_json::Value = client
        .post(format!("{}/api/sets", boot.base))
        .json(&serde_json::json!({ "repo": "a", "name": "s", "spans": [span("lib.rs")] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created_in_a["id"].as_str().unwrap();

    let cross_repo = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "b"), ("set", id)])
        .send()
        .await
        .unwrap();
    assert_eq!(cross_repo.status(), reqwest::StatusCode::NOT_FOUND);
}

// =====================================================================
// POST /api/sets/from-session (loopback-only)
// =====================================================================

/// Seed one project dir + one session transcript touching `files` (each an
/// `Edit` tool_use turn, absolute `file_path` under `repo_dir`) — the
/// from-session materializer's uncommitted-evidence fixture, mirroring
/// `sessiondiff::session_diff_tests`'s `user_line`/`tool_use_line` shape.
fn seed_session_touching(root: &Path, session_id: &str, repo_dir: &Path, files: &[&str]) {
    let proj = root.join("-fixture-project");
    std::fs::create_dir_all(&proj).unwrap();
    let mut lines = vec![format!(
        r#"{{"type":"user","uuid":"u0","parentUuid":null,"sessionId":"{session_id}","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{{"role":"user","content":"add the widget"}}}}"#,
    )];
    for (i, f) in files.iter().enumerate() {
        let abs = repo_dir.join(f).display().to_string();
        let abs_json = serde_json::to_string(&abs).unwrap();
        lines.push(format!(
            r#"{{"type":"assistant","uuid":"u{}","parentUuid":null,"sessionId":"{session_id}","timestamp":"2026-07-17T10:00:0{}.000Z","isSidechain":false,"message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t{i}","name":"Edit","input":{{"file_path":{abs_json}}}}}]}}}}"#,
            i + 1,
            i + 1,
        ));
    }
    std::fs::write(
        proj.join(format!("{session_id}.jsonl")),
        lines.join("\n") + "\n",
    )
    .unwrap();
}

async fn wait_for_transcript_turns(base: &str, expected: u64) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let body: serde_json::Value = client
            .get(format!("{base}/api/transcripts/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if body["turns"].as_u64().unwrap_or(0) >= expected || Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_session_materializes_uncommitted_evidence_in_first_touch_order() {
    let repo_tmp = fixture_repo();
    let transcripts_tmp = tempfile::tempdir().unwrap();
    let transcripts_root = transcripts_tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts_root).unwrap();
    seed_session_touching(
        &transcripts_root,
        "sess-1",
        repo_tmp.path(),
        &["lib.rs", "main.rs"],
    );

    let boot = boot(
        vec![RepoEntry {
            name: "fixture".to_string(),
            path: std::fs::canonicalize(repo_tmp.path()).unwrap(),
        }],
        Some(TranscriptsSection {
            enabled: true,
            root: transcripts_root.to_string_lossy().into_owned(),
            exclude_projects: Vec::new(),
            index_thinking: true,
        }),
    )
    .await;
    wait_for_transcript_turns(&boot.base, 3).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/sets/from-session", boot.base))
        .json(&serde_json::json!({ "repo": "fixture", "session_id": "sess-1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "session: add the widget");
    let spans = body["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0]["path"], "lib.rs");
    assert_eq!(spans[1]["path"], "main.rs");
    assert!(spans.iter().all(|s| s["line_start"].is_null()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_session_honours_an_explicit_name_and_409s_a_repeat() {
    let repo_tmp = fixture_repo();
    let transcripts_tmp = tempfile::tempdir().unwrap();
    let transcripts_root = transcripts_tmp.path().join("transcripts");
    std::fs::create_dir_all(&transcripts_root).unwrap();
    seed_session_touching(&transcripts_root, "sess-2", repo_tmp.path(), &["lib.rs"]);

    let boot = boot(
        vec![RepoEntry {
            name: "fixture".to_string(),
            path: std::fs::canonicalize(repo_tmp.path()).unwrap(),
        }],
        Some(TranscriptsSection {
            enabled: true,
            root: transcripts_root.to_string_lossy().into_owned(),
            exclude_projects: Vec::new(),
            index_thinking: true,
        }),
    )
    .await;
    wait_for_transcript_turns(&boot.base, 2).await;

    let client = reqwest::Client::new();
    let first = client
        .post(format!("{}/api/sets/from-session", boot.base))
        .json(&serde_json::json!({
            "repo": "fixture", "session_id": "sess-2", "name": "my custom name",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = first.json().await.unwrap();
    assert_eq!(body["name"], "my custom name");

    let second = client
        .post(format!("{}/api/sets/from-session", boot.base))
        .json(&serde_json::json!({
            "repo": "fixture", "session_id": "sess-2", "name": "my custom name",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_session_404s_an_unknown_session() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("fixture", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/sets/from-session", boot.base))
        .json(&serde_json::json!({ "repo": "fixture", "session_id": "no-such-session" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

// =====================================================================
// POST /api/sets/from-doc (loopback-only) — DCB-W3.C.R Major 1's own route
// tests, in the SAME harness as the `from_session_*` family above (real
// daemon, `boot`/`Boot`, `reqwest`), extended with `boot_from_doc` +
// `mock_kb_from_doc` for the one thing this route needs that `from-session`
// doesn't: a live (mocked) kb daemon to resolve against.
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_doc_materializes_present_refs_and_stamps_provenance_with_a_real_git_ref() {
    let repo_tmp = fixture_repo();
    let (kb_addr, _kb) = mock_kb_from_doc().await;
    let boot = boot_from_doc("fixture", repo_tmp.path(), kb_addr).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/sets/from-doc", boot.base))
        .json(&serde_json::json!({
            "repo": "fixture", "kb": "platform", "doc": "doc1", "name": "from doc",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["source_kb"], "platform");
    assert_eq!(body["source_doc_id"], "doc1");
    assert_eq!(body["source_doc_path"], "docs/x.html");
    assert_eq!(body["source_doc_hash"], "hash1");

    let spans = body["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0]["path"], "lib.rs");

    // DCB-W3.C.R Blocker 2 — a clean, freshly-committed fixture repo (see
    // `fixture_repo`'s own doc) resolves to a REAL revspec: the full 40-hex
    // `HEAD` sha, never a `"{repo}@{sha}[+dirty]"` label `GET
    // /api/file?ref=` can't resolve.
    let git_ref = spans[0]["ref"]
        .as_str()
        .expect("a clean resolve pins a ref");
    assert_eq!(git_ref.len(), 40, "full sha, not a label: {git_ref:?}");
    assert!(
        git_ref.chars().all(|c| c.is_ascii_hexdigit()),
        "must be a real revspec: {git_ref:?}"
    );

    // DCB-W3.C.R Blocker 1 — no half-range: `lib.rs`'s fixture ref carries no
    // line hint at all, so BOTH sides are absent, never a lone `line_start`.
    assert!(spans[0].get("line_start").is_none());
    assert!(spans[0].get("line_end").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_doc_404s_an_unknown_repo() {
    let repo_tmp = fixture_repo();
    let (kb_addr, _kb) = mock_kb_from_doc().await;
    let boot = boot_from_doc("fixture", repo_tmp.path(), kb_addr).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/sets/from-doc", boot.base))
        .json(&serde_json::json!({ "repo": "no-such-repo", "kb": "platform", "doc": "doc1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_doc_404s_an_unresolvable_doc() {
    let repo_tmp = fixture_repo();
    let (kb_addr, _kb) = mock_kb_from_doc().await;
    let boot = boot_from_doc("fixture", repo_tmp.path(), kb_addr).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/sets/from-doc", boot.base))
        .json(&serde_json::json!({ "repo": "fixture", "kb": "platform", "doc": "no-such-doc" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn from_doc_409s_an_explicit_name_collision() {
    let repo_tmp = fixture_repo();
    let (kb_addr, _kb) = mock_kb_from_doc().await;
    let boot = boot_from_doc("fixture", repo_tmp.path(), kb_addr).await;
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "repo": "fixture", "kb": "platform", "doc": "doc1", "name": "dup name",
    });

    let first = client
        .post(format!("{}/api/sets/from-doc", boot.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), reqwest::StatusCode::CREATED);

    let second = client
        .post(format!("{}/api/sets/from-doc", boot.base))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
}
