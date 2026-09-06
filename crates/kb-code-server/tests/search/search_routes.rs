//! W2.1 — end-to-end HTTP tests for the INSTANT search lanes
//! (`GET /api/search/{files,symbols,text}`), against a real daemon booted
//! via `serve_on_random_port_with_paths` over a real git fixture repo.
//! Mirrors `tests/e2e_daemon.rs`'s own conventions (`boot_with_repo`,
//! `wait_until_async`, the `SERIAL` guard) rather than sharing them — this
//! crate has no `tests/support` module yet, and each e2e file already
//! duplicates this small helper set rather than introducing one.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

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

async fn boot_with_repos(
    repos: &[(&str, &Path)],
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: repos
            .iter()
            .map(|(name, path)| RepoEntry {
                name: name.to_string(),
                path: std::fs::canonicalize(path).unwrap(),
            })
            .collect(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_files_finds_seeded_file_and_falls_back_to_recent_on_empty_query() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("gizmo_widget.rs"), b"fn make() {}\n").unwrap();
    std::fs::write(dir.join("other.rs"), b"fn other() {}\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/search/files"))
                .query(&[("repo", "fixture"), ("q", "gizmo")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["hits"]
                .as_array()
                .is_some_and(|hits| hits.iter().any(|h| h["path"] == "gizmo_widget.rs"))
        }
    })
    .await;
    assert!(
        ok,
        "expected the initial index walk to make the file findable via fuzzy search"
    );

    // Open the file once (bumps frecency), then an empty query must surface
    // it via the "recent" fallback.
    let _ = client
        .get(format!("{base}/api/file"))
        .query(&[("repo", "fixture"), ("path", "gizmo_widget.rs")])
        .send()
        .await
        .unwrap();
    let recent: serde_json::Value = client
        .get(format!("{base}/api/search/files"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let hits = recent["hits"].as_array().unwrap();
    assert!(
        hits.iter().any(|h| h["path"] == "gizmo_widget.rs"),
        "expected the opened file in the empty-query recent list: {hits:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_files_with_no_repo_scopes_across_every_configured_repo() {
    let _guard = SERIAL.lock().await;
    let repo_a_tmp = tempfile::tempdir().unwrap();
    let dir_a = std::fs::canonicalize(repo_a_tmp.path()).unwrap();
    init_repo(&dir_a);
    std::fs::write(dir_a.join("shared_needle.rs"), b"fn a() {}\n").unwrap();
    git(&dir_a, &["add", "-A"]);
    git(&dir_a, &["commit", "-q", "-m", "c1"]);

    let repo_b_tmp = tempfile::tempdir().unwrap();
    let dir_b = std::fs::canonicalize(repo_b_tmp.path()).unwrap();
    init_repo(&dir_b);
    std::fs::write(dir_b.join("shared_needle.rs"), b"fn b() {}\n").unwrap();
    git(&dir_b, &["add", "-A"]);
    git(&dir_b, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("a", &dir_a), ("b", &dir_b)]).await;
    let client = reqwest::Client::new();

    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/search/files"))
                .query(&[("q", "shared_needle")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["hits"].as_array().is_some_and(|h| h.len() == 2)
        }
    })
    .await;
    assert!(ok, "expected hits from BOTH repos when repo= is omitted");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_symbols_finds_seeded_symbol_and_400s_on_empty_query() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(
        dir.join("search.rs"),
        b"fn special_search_target() -> i32 {\n    42\n}\n",
    )
    .unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/search/symbols"))
                .query(&[("repo", "fixture"), ("q", "special_search")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["hits"]
                .as_array()
                .is_some_and(|h| h.iter().any(|s| s["name"] == "special_search_target"))
        }
    })
    .await;
    assert!(
        ok,
        "expected the fuzzy symbol lane to find the seeded function"
    );

    let status = client
        .get(format!("{base}/api/search/symbols"))
        .query(&[("repo", "fixture"), ("q", "")])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "empty q must 400");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_text_literal_and_regex_modes_and_400s_on_empty_query() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.rs"), b"fn needle_fn() {}\nfn other() {}\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/search/text"))
                .query(&[("repo", "fixture"), ("q", "needle_fn")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["results"]
                .as_array()
                .is_some_and(|r| r.iter().any(|f| f["path"] == "a.rs"))
        }
    })
    .await;
    assert!(
        ok,
        "expected the initial walk to make the text lane's working-tree read succeed"
    );

    let regex_body: serde_json::Value = client
        .get(format!("{base}/api/search/text"))
        .query(&[("repo", "fixture"), ("q", "^fn other"), ("regex", "true")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let results = regex_body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["matches"][0]["line"], "fn other() {}");

    let status = client
        .get(format!("{base}/api/search/text"))
        .query(&[("repo", "fixture"), ("q", "")])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST, "empty q must 400");
}

/// W2.6 ingest e2e — a fixture repo carrying one file per REGISTERED
/// language (`lang::ALL_LANG_IDS`: the eight pre-W2.6 languages plus the
/// three "next-tier" ones this Wave adds — Go, TOML, JSON — eleven files in
/// total, a superset of the milestone's "all nine languages" bar) must
/// index cleanly through the SAME initial-walk path every other e2e test
/// here exercises, and `GET /api/search/symbols` must find both a Go
/// function (`kind = "func"`) and a TOML key (`kind = "key"`, dotted path)
/// through the fuzzy symbol lane — proving the whole ingest→store→search
/// pipeline (not just `extract_symbols` in isolation) handles the new
/// languages end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_symbols_indexes_every_language_finds_go_func_and_toml_key() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.rs"), b"fn rust_probe_fn() {}\n").unwrap();
    std::fs::write(dir.join("a.py"), b"def python_probe_fn():\n    pass\n").unwrap();
    std::fs::write(dir.join("a.rb"), b"def ruby_probe_method\nend\n").unwrap();
    std::fs::write(
        dir.join("a.ts"),
        b"function typescript_probe_fn(): void {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("a.tsx"),
        b"function TsxProbeComponent() { return null; }\n",
    )
    .unwrap();
    std::fs::write(dir.join("a.js"), b"function javascript_probe_fn() {}\n").unwrap();
    std::fs::write(dir.join("a.sh"), b"bash_probe_fn() {\n  echo hi\n}\n").unwrap();
    std::fs::write(dir.join("a.yml"), b"top_key:\n  nested: 1\n").unwrap();
    std::fs::write(
        dir.join("a.go"),
        b"package main\n\nfunc go_probe_fn() int {\n\treturn 1\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("a.toml"), b"[package]\ntoml_probe_key = \"v\"\n").unwrap();
    std::fs::write(dir.join("a.json"), b"{\"json_probe_key\": 1}\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let go_ok = wait_until_async(Duration::from_secs(20), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/search/symbols"))
                .query(&[("repo", "fixture"), ("q", "go_probe_fn")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["hits"].as_array().is_some_and(|h| {
                h.iter()
                    .any(|s| s["name"] == "go_probe_fn" && s["kind"] == "func")
            })
        }
    })
    .await;
    assert!(go_ok, "expected the Go func to be indexed and findable");

    // The initial HEAD-tree walk indexes files one at a time (see
    // `ingest::walk_dir`) — the Go probe landing above does NOT guarantee
    // every other file (including `a.toml`, alphabetically later) has
    // finished, so this needs its OWN `wait_until_async`, not a one-shot
    // request right after the Go check.
    let toml_ok = wait_until_async(Duration::from_secs(20), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/search/symbols"))
                .query(&[("repo", "fixture"), ("q", "toml_probe_key")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["hits"].as_array().is_some_and(|h| {
                h.iter()
                    .any(|s| s["name"] == "package.toml_probe_key" && s["kind"] == "key")
            })
        }
    })
    .await;
    assert!(toml_ok, "expected the TOML key to be indexed and findable");
}
