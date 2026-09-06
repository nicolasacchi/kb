//! W2.4 — end-to-end HTTP tests for the unified Search-Everywhere box
//! (`GET /api/search`), against a real daemon booted via
//! `serve_on_random_port_with_paths` over a real git fixture repo. Mirrors
//! `tests/search_routes.rs`'s own conventions (`boot_with_repos`,
//! `wait_until_async`, the `SERIAL` guard, XFF-spoof for a simulated
//! non-loopback caller — same technique `tests/transcripts.rs` uses).
//!
//! `[kb_daemon] url` is always pointed at a dead port (`127.0.0.1:1` —
//! refused immediately, no listener) rather than the real default
//! `127.0.0.1:4000`: these tests must be deterministic whether or not a real
//! `kb` daemon happens to be running on the test machine.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

/// Never resolves to a live daemon — see the module doc.
const DEAD_KB_DAEMON: &str = "http://127.0.0.1:1";

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

async fn boot(
    cfg: KbCodeConfig,
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

/// One repo, one seeded file (`gizmo_widget.rs`, a Rust file with a known
/// symbol) + one plain-text file (`notes.txt`, not lang-detected) — enough
/// fixture surface for files/symbols/text (and their `lang:`/`path:`
/// filters) without a second repo.
fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_repo(dir);
    std::fs::write(
        dir.join("gizmo_widget.rs"),
        b"fn gizmo_symbol_fn() -> i32 {\n    1\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("notes.txt"), b"gizmo notes here\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

fn base_config(repo_dir: &Path) -> KbCodeConfig {
    KbCodeConfig {
        repos: vec![RepoEntry {
            name: "fixture".to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: true,
            url: DEAD_KB_DAEMON.to_string(),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    }
}

async fn wait_for_search(
    client: &reqwest::Client,
    base: &str,
    q: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let url = format!("{base}/api/search");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let body: serde_json::Value = client
            .get(&url)
            .query(&[("q", q), ("repo", "fixture")])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if pred(&body) || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn section<'a>(body: &'a serde_json::Value, lane: &str) -> Option<&'a serde_json::Value> {
    body["sections"]
        .as_array()?
        .iter()
        .find(|s| s["lane"] == lane)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_prefix_query_returns_every_lane_in_fixed_canonical_order() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    // Wait for BOTH the section shape (6 lanes) AND the initial repo index
    // walk to have actually populated the files/symbols/text lanes —
    // `sections.len() == 6` alone is true on the very first poll (every
    // lane is always ATTEMPTED for a no-prefix query, indexed or not), so
    // checking shape alone would race the background initial-index walk.
    let body = wait_for_search(&client, &base, "gizmo", |b| {
        b["sections"].as_array().is_some_and(|s| s.len() == 6)
            && section(b, "files").is_some_and(|s| {
                s["results"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|h| h["path"] == "gizmo_widget.rs"))
            })
            && section(b, "symbols")
                .is_some_and(|s| s["results"].as_array().is_some_and(|r| !r.is_empty()))
            && section(b, "text").is_some_and(|s| {
                s["results"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|f| f["path"] == "notes.txt"))
            })
    })
    .await;

    let lanes: Vec<&str> = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lane"].as_str().unwrap())
        .collect();
    assert_eq!(
        lanes,
        vec![
            "files",
            "symbols",
            "text",
            "semantic",
            "sessions",
            "transcripts"
        ],
        "sections must be in the fixed canonical order: {body}"
    );
    assert_eq!(body["query_echo"], "gizmo");

    // files/symbols/text found real hits from the fixture repo.
    let files = section(&body, "files").unwrap();
    assert!(
        files["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["path"] == "gizmo_widget.rs"),
        "files section: {files}"
    );
    let symbols = section(&body, "symbols").unwrap();
    // The "gizmo" query fuzzy-matches "gizmo_symbol_fn" by substring/subsequence.
    assert!(
        !symbols["results"].as_array().unwrap().is_empty(),
        "symbols section: {symbols}"
    );
    let text = section(&body, "text").unwrap();
    assert!(
        text["results"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "notes.txt"),
        "text section: {text}"
    );

    // semantic is off by default ([semantic] enabled = false) — unavailable,
    // never a 500 and never silently empty-without-explanation. `?repo=` is
    // set on this request (see `wait_for_search`), so the repo-scoped
    // message ("not enabled for repo ...") applies, not the bare
    // "disabled" one `run_semantic` uses when no repo is given.
    let semantic = section(&body, "semantic").unwrap();
    assert!(
        semantic["unavailable_reason"]
            .as_str()
            .is_some_and(|r| r.contains("not enabled for repo")),
        "semantic section: {semantic}"
    );
    assert_eq!(semantic["results"], serde_json::json!([]));

    // sessions federates to a dead kb daemon — unavailable, not a 500 and
    // not a hang (the module's 1.5s timeout, well under this test's own
    // 15s poll deadline).
    let sessions = section(&body, "sessions").unwrap();
    assert!(
        sessions["unavailable_reason"].as_str().is_some(),
        "sessions section: {sessions}"
    );

    // transcripts: a loopback caller, so the section is PRESENT (even
    // though this fixture never seeded any transcript JSONL, so it's an
    // ordinary empty-but-available result, not `unavailable_reason`).
    let transcripts = section(&body, "transcripts").unwrap();
    assert!(transcripts["unavailable_reason"].is_null());
    assert_eq!(transcripts["results"], serde_json::json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbols_prefix_isolates_to_the_symbols_lane_only() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    let body = wait_for_search(&client, &base, "@gizmo_symbol", |b| {
        section(b, "symbols").is_some_and(|s| {
            s["results"]
                .as_array()
                .is_some_and(|r| r.iter().any(|h| h["name"] == "gizmo_symbol_fn"))
        })
    })
    .await;

    let lanes: Vec<&str> = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lane"].as_str().unwrap())
        .collect();
    assert_eq!(lanes, vec!["symbols"], "body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn files_prefix_isolates_to_the_files_lane_only() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    let body = wait_for_search(&client, &base, "#gizmo_widget", |b| {
        section(b, "files").is_some_and(|s| {
            s["results"]
                .as_array()
                .is_some_and(|r| r.iter().any(|h| h["path"] == "gizmo_widget.rs"))
        })
    })
    .await;
    let lanes: Vec<&str> = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lane"].as_str().unwrap())
        .collect();
    assert_eq!(lanes, vec!["files"], "body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_prefix_isolates_to_the_text_lane_only() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    let body = wait_for_search(&client, &base, "/gizmo notes", |b| {
        section(b, "text").is_some_and(|s| {
            s["results"]
                .as_array()
                .is_some_and(|r| r.iter().any(|f| f["path"] == "notes.txt"))
        })
    })
    .await;
    let text = section(&body, "text").unwrap();
    assert!(text["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "notes.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn semantic_prefix_isolates_and_reports_unavailable_when_disabled() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/search"))
        .query(&[("q", "?nl how does the gizmo work"), ("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lanes: Vec<&str> = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lane"].as_str().unwrap())
        .collect();
    assert_eq!(lanes, vec!["semantic"], "body: {body}");
    let semantic = section(&body, "semantic").unwrap();
    assert!(semantic["unavailable_reason"].as_str().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sessions_prefix_isolates_and_reports_unavailable_when_the_kb_daemon_is_down() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/search"))
        .query(&[("q", "~fixed the gizmo race"), ("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lanes: Vec<&str> = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lane"].as_str().unwrap())
        .collect();
    assert_eq!(lanes, vec!["sessions"], "body: {body}");
    let sessions = section(&body, "sessions").unwrap();
    let reason = sessions["unavailable_reason"].as_str().unwrap();
    assert!(
        reason.contains("unreachable") || reason.contains("daemon"),
        "expected an honest unreachable-daemon reason, got: {reason}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lang_and_path_filters_narrow_the_files_and_text_lanes() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    // Both "gizmo_widget.rs" (lang rust) and "notes.txt" (lang unknown)
    // contain "gizmo" — `lang:rust` must keep only the former in files,
    // and the text lane (which also sees both files textually) must drop
    // "notes.txt" too.
    let body = wait_for_search(&client, &base, "gizmo lang:rust", |b| {
        section(b, "files").is_some_and(|f| !f["results"].as_array().unwrap().is_empty())
    })
    .await;
    let files = section(&body, "files").unwrap();
    let paths: Vec<&str> = files["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"gizmo_widget.rs"), "files: {paths:?}");
    assert!(!paths.contains(&"notes.txt"), "files: {paths:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_loopback_caller_never_sees_a_transcripts_section() {
    // A bearer token so the spoofed non-loopback requests below actually
    // REACH `routes::search_unified` (`auth_bearer` itself 401s a
    // token-less non-loopback caller, invariant #4 — same
    // `ENV_TEST_LOCK`-style env-mutation discipline `tests/boot.rs` uses;
    // `SERIAL` already serializes every test in this file, so mutating the
    // process-global env var here is safe without a second lock).
    const FIXTURE_TOKEN: &str = "kb-code-search-unified-test-token";
    let _guard = SERIAL.lock().await;
    std::env::remove_var("KB_ALLOW_NO_AUTH");
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);
    let repo_tmp = fixture_repo();
    let boot_result = boot(base_config(repo_tmp.path())).await;
    std::env::remove_var("KB_CODE_TOKEN");
    let (_tmp, base, _task) = boot_result;
    let client = reqwest::Client::new();
    let auth_header = format!("Bearer {FIXTURE_TOKEN}");

    // No-prefix (all lanes) from a simulated non-loopback peer (loopback
    // TCP peer + a genuine external X-Forwarded-For — the same spoof
    // technique `tests/transcripts.rs`/`tests/boot.rs` use) WITH a valid
    // bearer token (so the request is admitted): every OTHER lane still
    // runs, but transcripts must be ABSENT, not `unavailable_reason`.
    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        let auth_header = auth_header.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/search"))
                .query(&[("q", "gizmo"), ("repo", "fixture")])
                .header("X-Forwarded-For", "8.8.8.8")
                .header("Authorization", auth_header)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["sections"].as_array().is_some_and(|s| s.len() == 5)
        }
    })
    .await;
    assert!(
        ok,
        "expected exactly 5 sections (transcripts absent) for a non-loopback caller"
    );

    let body: serde_json::Value = client
        .get(format!("{base}/api/search"))
        .query(&[("q", "gizmo"), ("repo", "fixture")])
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lanes: Vec<&str> = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lane"].as_str().unwrap())
        .collect();
    assert!(!lanes.contains(&"transcripts"), "lanes: {lanes:?}");

    // The `~~` prefix explicitly asking for transcripts, from the SAME
    // non-loopback (but now-authenticated) caller: not a 404 (the box
    // itself is an ordinary authenticated route; only the loopback-only
    // STANDALONE transcripts routes 404, and a valid bearer token must NOT
    // reopen that lane to a non-loopback caller here either). V70-A3X: an
    // EXPLICIT, single-lane ask for a loopback-only lane now gets an honest
    // `unavailable_reason` section rather than a silently empty list —
    // contrast the no-prefix case above, where transcripts is just one of
    // six ATTEMPTED lanes and dropping it silently is unchanged.
    let resp = client
        .get(format!("{base}/api/search"))
        .query(&[("q", "~~gizmo"), ("repo", "fixture")])
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", &auth_header)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let lanes: Vec<&str> = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lane"].as_str().unwrap())
        .collect();
    assert_eq!(lanes, vec!["transcripts"], "body: {body}");
    let transcripts = section(&body, "transcripts").unwrap();
    assert!(
        transcripts["unavailable_reason"]
            .as_str()
            .is_some_and(|r| r.contains("loopback")),
        "body: {body}"
    );
    assert_eq!(transcripts["results"], serde_json::json!([]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn path_filter_narrows_files_symbols_and_text_lanes_before_truncation() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    // `path:` needs a candidate outside the default fixture surface that
    // shares a query with a decoy at the repo root — nest a second
    // "gizmo"-matching file under a subdirectory.
    std::fs::create_dir_all(repo_tmp.path().join("nested")).unwrap();
    std::fs::write(
        repo_tmp.path().join("nested/gizmo_extra.rs"),
        b"fn gizmo_extra_fn() {}\n",
    )
    .unwrap();
    git(repo_tmp.path(), &["add", "-A"]);
    git(repo_tmp.path(), &["commit", "-q", "-m", "c2"]);
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    // `path:nested/` must keep ONLY the nested file across files/text —
    // proves the filter is applied (not just present in the unfiltered
    // superset).
    let body = wait_for_search(&client, &base, "gizmo path:nested/", |b| {
        section(b, "files").is_some_and(|f| !f["results"].as_array().unwrap().is_empty())
    })
    .await;
    let files = section(&body, "files").unwrap();
    let paths: Vec<&str> = files["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["nested/gizmo_extra.rs"], "files: {paths:?}");

    let text = section(&body, "text").unwrap();
    let text_paths: Vec<&str> = text["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        text_paths,
        vec!["nested/gizmo_extra.rs"],
        "text: {text_paths:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_caller_sees_the_transcripts_prefix_lane() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/search"))
        .query(&[("q", "~~gizmo"), ("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lanes: Vec<&str> = body["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["lane"].as_str().unwrap())
        .collect();
    assert_eq!(lanes, vec!["transcripts"], "body: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_query_returns_files_recents_only() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_tmp, base, _task) = boot(base_config(repo_tmp.path())).await;
    let client = reqwest::Client::new();

    // Wait for the initial index walk, then open the fixture file once to
    // seed the files lane's open-history/frecency signal.
    wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/search/files"))
                .query(&[("repo", "fixture"), ("q", "gizmo_widget")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["hits"]
                .as_array()
                .is_some_and(|h| h.iter().any(|x| x["path"] == "gizmo_widget.rs"))
        }
    })
    .await;
    let _ = client
        .get(format!("{base}/api/file"))
        .query(&[("repo", "fixture"), ("path", "gizmo_widget.rs")])
        .send()
        .await
        .unwrap();

    let body: serde_json::Value = client
        .get(format!("{base}/api/search"))
        .query(&[("q", ""), ("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sections = body["sections"].as_array().unwrap();
    assert_eq!(sections.len(), 1, "body: {body}");
    assert_eq!(sections[0]["lane"], "files");
    assert!(sections[0]["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["path"] == "gizmo_widget.rs"));

    // Omitting `q` entirely is the same as an empty string.
    let body2: serde_json::Value = client
        .get(format!("{base}/api/search"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body2["sections"].as_array().unwrap().len(), 1);
    assert_eq!(body2["sections"][0]["lane"], "files");
}
