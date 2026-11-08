//! A5 — end-to-end HTTP tests for `GET /api/session-card` (`kbc-session-card/1`)
//! against a real daemon booted over a mock kb that serves `session_commits`
//! and the COMMITTED live-touches fixture (`tests/fixtures/live-touches-v1.json`).
//!
//! Pins: the four lanes answer in one read; an unknown sid is 200 with empty
//! lanes (never 404); a malformed sid / unknown repo filter is 400 without
//! echoing the sid; kb failures degrade only the kb lanes (and kb failure
//! with cache rows still serves the cache); repeat reads are byte-identical
//! modulo the clock; the route is bearer-gated off-loopback and GET-only;
//! reviews authored through `review_docs` carry basis `doc_author`.

use axum::{extract::Path as AxPath, routing::get, Json, Router};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::common::{git, init_repo};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/live-touches-v1.json");
/// The fixture's own first sid: its two touches (`src/lib.rs` age 4,
/// `docs/readme.md` age 120) map through the `demo` alias onto `fixture`.
const SID: &str = "11111111-aaaa-4bbb-8ccc-000000000001";

fn hello_ok() -> Value {
    json!({
        "name": "kb",
        "sibling_protocol": kb_core::sibling::SIBLING_PROTOCOL,
        "sibling_major": kb_core::sibling::SIBLING_MAJOR
    })
}

async fn spawn_mock(identity: Value, commits_body: Arc<Mutex<String>>) -> SocketAddr {
    let router = Router::new()
        .route(
            "/api/identity",
            get(move || {
                let identity = identity.clone();
                async move { Json(identity) }
            }),
        )
        .route(
            "/api/sessions/:sid/commits",
            get(move |AxPath(_sid): AxPath<String>| {
                let commits_body = commits_body.clone();
                async move {
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        commits_body.lock().unwrap().clone(),
                    )
                }
            }),
        )
        .route(
            "/api/sessions/live-touches",
            get(move || async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    FIXTURE,
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}

struct Boot {
    _tmp: tempfile::TempDir,
    _repo: tempfile::TempDir,
    base: String,
    store_path: PathBuf,
    tip_sha: String,
    _task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// Boot with one commit carrying `Kb-Session: {SID}` and kb pointing at the
/// caller-visible mock address (or disabled).
async fn boot(kb: KbDaemonSection) -> Boot {
    boot_with_trailer(kb, SID).await
}

async fn boot_with_trailer(kb: KbDaemonSection, trailer: &str) -> Boot {
    let repo = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"x\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            &format!("fixture subject\n\nKb-Session: {trailer}\n"),
        ],
    );
    let tip_sha = git_out(&dir, &["rev-parse", "HEAD"]);
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: dir.clone(),
            projects: vec!["demo".to_string()],
        }],
        kb_daemon: kb,
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let store_path = paths.state.join("index.db");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    Boot {
        _tmp: tmp,
        _repo: repo,
        base: format!("http://{addr}"),
        store_path,
        tip_sha,
        _task: task,
    }
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {:?} failed", args);
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn enabled(kb: SocketAddr) -> KbDaemonSection {
    KbDaemonSection {
        enabled: true,
        url: Some(format!("http://{kb}")),
        token_file: None,
        public_url: None,
    }
}

fn disabled() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: None,
        token_file: None,
        public_url: None,
    }
}

async fn get_json(base: &str, path: &str) -> (reqwest::StatusCode, Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn open_store(path: &Path) -> kb_code_server::store::Store {
    kb_code_server::store::Store::open(path).expect("open store")
}

fn seed_review(store: &kb_code_server::store::Store, session_id: Option<&str>) -> i64 {
    store
        .create_review(
            "fixture",
            Some("Add fixture review"),
            "main",
            "feature",
            session_id,
            1_700_000_000,
        )
        .unwrap()
}

fn commits_body_for(repo_dir: &Path, sha: &str) -> String {
    serde_json::to_string(&json!({
        "commits": [{
            "kind": "commit",
            "sha": sha,
            "subject": "fixture subject",
            "resolved": true,
            "sha_full": sha,
            "repo_root": repo_dir.to_str().unwrap(),
            "author": "fixture",
            "parents": 0,
            "trailers": [],
        }]
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_card_happy_path_returns_all_four_lanes() {
    let repo = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"x\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(
        &dir,
        &[
            "commit",
            "-q",
            "-m",
            &format!("fixture subject\n\nKb-Session: {SID}\n"),
        ],
    );
    let sha = git_out(&dir, &["rev-parse", "HEAD"]);
    let commits_body = Arc::new(Mutex::new(commits_body_for(&dir, &sha)));
    let kb = spawn_mock(hello_ok(), commits_body.clone()).await;

    let b = boot_with_repo(enabled(kb), &dir, &repo, &sha).await;

    // Seed the local side: one review started by this session (with a
    // verdict + PR binding) and one finding it authored.
    {
        let store = open_store(&b.store_path);
        let id = seed_review(&store, Some(SID));
        store
            .set_review_pr_binding(id, 12, "acme/widget", None, None, None)
            .unwrap();
        store
            .set_review_verdict(id, "approve", None, 1_700_000_000, 2)
            .unwrap();
        let repo_id = store.repo_id("fixture").unwrap().unwrap();
        let finding = kb_code_server::store::NewReviewFinding {
            review_id: id,
            repo_id,
            ps_number: 1,
            slug: "f-one".to_string(),
            severity: kb_code_server::store::SEVERITY_CONCERN.to_string(),
            category: "Style".to_string(),
            location_kind: kb_code_server::store::LOCATION_KIND_SINGLE.to_string(),
            location_path: "src/lib.rs".to_string(),
            location_lines: None,
            location_removed: false,
            title: "fixture finding".to_string(),
            rationale: "fixture rationale".to_string(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            anchor_kind: "line".to_string(),
            anchor: serde_json::to_string(&kb_code_server::annotations::anchor_for_line(1, "x"))
                .unwrap(),
            anchor2: None,
            side: Some("new".to_string()),
            author: "KB".to_string(),
            import_batch_id: "fixture".to_string(),
            origin: kb_code_server::store::FINDING_ORIGIN_MANUAL.to_string(),
            finding_author: Some(format!("session:{SID}")),
            act: "issue".to_string(),
            blocking: false,
            cites_json: None,
            fingerprint: None,
        };
        store
            .insert_review_finding(&finding, 1_700_000_000)
            .unwrap();
    }

    let (status, body) = get_json(&b.base, &format!("/api/session-card?sid={SID}")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["schema"], "kbc-session-card/1");
    assert_eq!(body["sid"], SID);

    let commits = &body["commits"];
    assert_eq!(commits["available"], true, "{body}");
    assert_eq!(commits["total"], 1);
    assert_eq!(commits["shown"], 1);
    assert_eq!(commits["truncated"], false);
    let item = &commits["items"][0];
    assert_eq!(item["repo"], "fixture");
    assert_eq!(item["sha"], sha);
    assert_eq!(item["visible"], true);
    assert_eq!(item["confidence"], "trailer");
    assert_eq!(item["attributed_here"], true);

    let reviews = &body["reviews"];
    assert_eq!(reviews["available"], true, "{body}");
    assert_eq!(reviews["total"], 1);
    let review = &reviews["items"][0];
    assert_eq!(review["title"], "Add fixture review");
    assert_eq!(review["state"], "open");
    assert_eq!(review["verdict"], "approve");
    assert_eq!(review["verdict_ps"], 2);
    assert_eq!(review["pr_number"], 12);
    assert_eq!(review["basis"], "session_id");

    let findings = &body["findings"];
    assert_eq!(findings["available"], true, "{body}");
    assert_eq!(findings["total"], 1);
    assert_eq!(findings["by_severity"]["concern"], 1);
    assert_eq!(findings["basis"], "author");

    let presence = &body["presence"];
    assert_eq!(presence["available"], true, "{body}");
    // The fixture's two touches for this sid, through the `demo` alias.
    assert_eq!(presence["files"], 2, "{body}");
    assert_eq!(presence["newest_age_secs"], 4);

    // No repo-relative paths or filesystem roots anywhere in the body.
    let text = body.to_string();
    assert!(!text.contains("src/lib.rs"), "{text}");
    assert!(!text.contains(dir.to_str().unwrap()), "{text}");
}

async fn boot_with_repo(
    kb: KbDaemonSection,
    repo_dir: &std::path::Path,
    _repo: &tempfile::TempDir,
    sha: &str,
) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: repo_dir.to_path_buf(),
            projects: vec!["demo".to_string()],
        }],
        kb_daemon: kb,
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let store_path = paths.state.join("index.db");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    Boot {
        _tmp: tmp,
        _repo: tempfile::tempdir().unwrap(),
        base: format!("http://{addr}"),
        store_path,
        repo_dir: repo_dir.to_path_buf(),
        tip_sha: sha.to_string(),
        _task: task,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_sid_is_200_with_empty_lanes() {
    let commits_body = Arc::new(Mutex::new(
        serde_json::to_string(&json!({ "commits": [] })).unwrap(),
    ));
    let kb = spawn_mock(hello_ok(), commits_body).await;
    let b = boot(enabled(kb)).await;

    let (status, body) = get_json(&b.base, "/api/session-card?sid=sess-0001").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["schema"], "kbc-session-card/1");
    assert_eq!(body["sid"], "sess-0001");
    assert_eq!(body["commits"]["available"], true);
    assert_eq!(body["commits"]["total"], 0);
    assert_eq!(body["commits"]["items"].as_array().unwrap().len(), 0);
    assert_eq!(body["reviews"]["available"], true);
    assert_eq!(body["reviews"]["total"], 0);
    assert_eq!(body["findings"]["available"], true);
    assert_eq!(body["findings"]["total"], 0);
    assert_eq!(body["presence"]["available"], true);
    assert_eq!(body["presence"]["files"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_sid_is_400_and_unknown_repo_is_400() {
    let commits_body = Arc::new(Mutex::new(
        serde_json::to_string(&json!({ "commits": [] })).unwrap(),
    ));
    let kb = spawn_mock(hello_ok(), commits_body).await;
    let b = boot(enabled(kb)).await;

    // A slash in the sid must be a 400 — and the rejected sid is never
    // echoed back.
    let (status, body) = get_json(&b.base, "/api/session-card?sid=a%2Fb").await;
    assert_eq!(status, 400, "{body}");
    assert!(
        !body.to_string().contains("a/b"),
        "rejected sid must not be echoed: {body}"
    );

    // Missing sid entirely.
    let (status, _) = get_json(&b.base, "/api/session-card").await;
    assert_eq!(status, 400);

    // Unknown repo filter.
    let (status, body) = get_json(&b.base, "/api/session-card?sid=sess-0001&repo=nope").await;
    assert_eq!(status, 400, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kb_down_degrades_only_the_kb_lanes() {
    // kb unreachable: commits + presence degrade; reviews + findings are
    // local and stay available. The commits cache is seeded too, so the
    // commits lane still serves the cached row with the kb failure caption.
    let b = boot(KbDaemonSection {
        enabled: true,
        url: Some("http://127.0.0.1:1".to_string()),
        token_file: None,
        public_url: None,
    })
    .await;
    {
        let store = open_store(&b.store_path);
        let repo_id = store.repo_id("fixture").unwrap().unwrap();
        let row = kb_code_server::store::CommitSessionRow {
            confidence: "exact".to_string(),
            via: "by-commit".to_string(),
            session_id: Some(SID.to_string()),
            kb: Some("platform".to_string()),
            display_name: Some("fixture".to_string()),
            started_at: Some(1_700_000_000),
            resolved_at: 1_700_000_100,
        };
        store
            .upsert_commit_session(repo_id, &b.tip_sha, &row)
            .unwrap();
        seed_review(&store, Some(SID));
    }

    let (status, body) = get_json(&b.base, &format!("/api/session-card?sid={SID}")).await;
    assert_eq!(status, 200, "{body}");
    // Cache has rows: available with the cached row + the kb failure caption.
    assert_eq!(body["commits"]["available"], true, "{body}");
    assert_eq!(body["commits"]["caption"], "kb unreachable");
    assert_eq!(body["commits"]["total"], 1);
    assert_eq!(body["commits"]["items"][0]["confidence"], "exact");
    assert_eq!(body["commits"]["items"][0]["attributed_here"], true);
    // Presence is kb-only: unavailable with the same caption.
    assert_eq!(body["presence"]["available"], false, "{body}");
    assert_eq!(body["presence"]["caption"], "kb unreachable");
    // Local lanes unaffected.
    assert_eq!(body["reviews"]["available"], true, "{body}");
    assert_eq!(body["reviews"]["total"], 1);
    assert_eq!(body["findings"]["available"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kb_disabled_gives_the_kb_disabled_caption() {
    let b = boot(disabled()).await;
    let (status, body) = get_json(&b.base, "/api/session-card?sid=sess-0001").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["commits"]["available"], false, "{body}");
    assert_eq!(body["commits"]["caption"], "kb disabled");
    assert_eq!(body["presence"]["available"], false, "{body}");
    assert_eq!(body["presence"]["caption"], "kb disabled");
    // Local lanes stay available.
    assert_eq!(body["reviews"]["available"], true);
    assert_eq!(body["findings"]["available"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeat_reads_are_byte_identical_modulo_clock() {
    let commits_body = Arc::new(Mutex::new(
        serde_json::to_string(&json!({ "commits": [] })).unwrap(),
    ));
    let kb = spawn_mock(hello_ok(), commits_body).await;
    let b = boot(enabled(kb)).await;
    {
        let store = open_store(&b.store_path);
        seed_review(&store, Some("sess-0001"));
    }

    let (_, first) = get_json(&b.base, "/api/session-card?sid=sess-0001").await;
    let (_, second) = get_json(&b.base, "/api/session-card?sid=sess-0001").await;
    let mut first = first;
    let mut second = second;
    first["now_unix"] = Value::Null;
    first["presence"]["newest_age_secs"] = Value::Null;
    second["now_unix"] = Value::Null;
    second["presence"]["newest_age_secs"] = Value::Null;
    assert_eq!(first, second);
}

static ENV_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn route_requires_bearer_off_loopback() {
    const FIXTURE_TOKEN: &str = "kb-code-test-token-fixture";
    let _guard = ENV_TEST_LOCK.lock().await;
    std::env::remove_var("KB_ALLOW_NO_AUTH");
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);
    let tmp = tempfile::tempdir().unwrap();
    let boot = kb_code_server::serve_on_random_port_with_paths(
        KbCodeConfig::default(),
        KbPaths::rooted_at(tmp.path(), "kb-code"),
    )
    .await;
    std::env::remove_var("KB_CODE_TOKEN");
    let (addr, _task) = boot.expect("serve with KB_CODE_TOKEN set");
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{addr}/api/session-card?sid=sess-0001"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "no bearer on a non-loopback caller");

    let resp = client
        .post(format!("http://{addr}/api/session-card?sid=sess-0001"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 405, "the card route is GET-only");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn doc_author_basis_is_found_through_review_docs() {
    let commits_body = Arc::new(Mutex::new(
        serde_json::to_string(&json!({ "commits": [] })).unwrap(),
    ));
    let kb = spawn_mock(hello_ok(), commits_body).await;
    let b = boot(enabled(kb)).await;
    {
        let store = open_store(&b.store_path);
        // One review the session STARTED (basis session_id) and one it only
        // AUTHORED the document for (basis doc_author).
        seed_review(&store, Some(SID));
        let doc_review = store
            .create_review(
                "fixture",
                Some("Doc authored review"),
                "main",
                "feature",
                None,
                1_700_000_000,
            )
            .unwrap();
        store
            .set_review_verdict(doc_review, "approve", None, 1_700_000_000, 2)
            .unwrap();
        drop(store);
        // No public doc-revision insert exists; the revision row is seeded
        // directly, the same way `revision` numbering is derived in the
        // store's own insert path.
        let conn = rusqlite::Connection::open(&b.store_path).unwrap();
        let author_json = serde_json::to_string(&json!({
            "kind": "agent",
            "session_id": SID,
            "considered": [],
            "not_considered": []
        }))
        .unwrap();
        conn.execute(
            "INSERT INTO review_docs
                (review_id, ps_number, revision, schema, tier, doc_md,
                 summary_md, risk_level, risk_why, omitted_json, author_json,
                 byte_len, created_at)
             VALUES (?1, 1, 1, 'kbc-review/1', 'v1', '# doc', 'summary',
                     NULL, NULL, '[]', ?2, 5, 1_700_000_000)",
            rusqlite::params![doc_review, author_json],
        )
        .unwrap();
    }

    let (status, body) = get_json(&b.base, &format!("/api/session-card?sid={SID}")).await;
    assert_eq!(status, 200, "{body}");
    let reviews = body["reviews"]["items"].as_array().unwrap();
    assert_eq!(body["reviews"]["total"], 2, "{body}");
    let by_title = |title: &str| reviews.iter().find(|r| r["title"] == title).unwrap();
    assert_eq!(by_title("Add fixture review")["basis"], "session_id");
    assert_eq!(by_title("Doc authored review")["basis"], "doc_author");
    assert_eq!(by_title("Doc authored review")["verdict"], "approve");
}
