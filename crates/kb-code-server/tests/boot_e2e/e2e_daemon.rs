//! W1.6 — end-to-end: `bind_and_spawn` wires the real store + git +
//! live-mirror sink + SSE bus together into a browsable daemon. Real git
//! subprocess fixtures (mirrors `tests/mirror_matrix.rs`'s own convention)
//! plus a real background watcher thread — polling/settling helpers, never
//! a fixed sleep-and-pray for the assertion itself.
//!
//! Serialized (see `tests/mirror_matrix.rs`'s own `SERIAL` rationale): each
//! `#[test]` fn here spins up a REAL daemon with a real background watcher
//! thread and real `git` subprocesses; running several at once starves each
//! other for CPU under a loaded box, which is a test-harness artifact, not a
//! property of the daemon itself.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use kb_code_server::config::{KbCodeConfig, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

use crate::common::{git, init_repo};
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

/// Poll `f` (an async predicate) until it returns `true` or `timeout`
/// elapses — never a fixed sleep-and-pray for the assertion itself.
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

async fn boot_with_repo(
    repo_path: &Path,
    repo_name: &str,
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_path).unwrap(),
        }],
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

/// Opens `<base>/api/events` and continuously appends raw bytes into a
/// shared buffer — a crude but sufficient substring-search fixture for
/// asserting "an SSE client observed frame X" without re-deriving a full
/// SSE frame parser in a test file (`kb-code-cli`'s `sse` module already
/// owns that for the CLI side).
async fn open_events_buffer(base: &str) -> Arc<AsyncMutex<String>> {
    let buf = Arc::new(AsyncMutex::new(String::new()));
    let buf2 = buf.clone();
    let url = format!("{base}/api/events");
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("connect SSE");
    tokio::spawn(async move {
        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        while let Some(Ok(chunk)) = stream.next().await {
            let mut b = buf2.lock().await;
            b.push_str(&String::from_utf8_lossy(&chunk));
        }
    });
    buf
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn boot_indexes_repo_and_reports_it_via_api_repos() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(
        dir.join("lib.rs"),
        b"fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("app.py"), b"def hi():\n    pass\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repo(&dir, "fixture").await;
    let client = reqwest::Client::new();

    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/repos"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["repos"][0]["file_count"].as_u64().unwrap_or(0) >= 2
        }
    })
    .await;
    assert!(ok, "expected the initial boot walk to populate file_count");

    let body: serde_json::Value = client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repo = &body["repos"][0];
    assert_eq!(repo["name"], "fixture");
    assert!(repo["symbol_count"].as_u64().unwrap_or(0) >= 1);
    assert_eq!(repo["head"]["branch"], "main");
    assert!(repo["head"]["sha"].as_str().is_some());
    assert!(
        repo["watcher"] == "watching" || repo["watcher"] == "polling",
        "got: {repo:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_edit_updates_file_content_and_emits_mirror_updated_sse() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(
        dir.join("lib.rs"),
        b"fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repo(&dir, "fixture").await;
    let client = reqwest::Client::new();

    // Let the initial boot walk settle before opening the SSE tap, so the
    // buffer below only has to observe the NEW edit's event.
    assert!(
        wait_until_async(Duration::from_secs(15), || {
            let client = client.clone();
            let base = base.clone();
            async move {
                let body: serde_json::Value = client
                    .get(format!("{base}/api/repos"))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                body["repos"][0]["file_count"].as_u64().unwrap_or(0) >= 1
            }
        })
        .await
    );

    let events_buf = open_events_buffer(&base).await;
    // Let the SSE connection establish + drain the startup replay tail
    // (head_moved + full_reconcile from the watcher's own boot reconcile).
    tokio::time::sleep(Duration::from_millis(300)).await;
    events_buf.lock().await.clear();

    std::fs::write(
        dir.join("lib.rs"),
        b"fn add(a: i32, b: i32, c: i32) -> i32 {\n    a + b + c\n}\nfn extra() {}\n",
    )
    .unwrap();

    // `content` updates the instant the fs write completes (`/api/file`'s
    // no-ref path is a direct `fs::read`, independent of the store — see
    // `routes::file`'s doc) — long before the debounced watcher (250ms) has
    // even noticed, let alone before the sink's async worker has derived
    // and persisted symbols/highlights. Wait for ALL THREE together so this
    // poll only succeeds once the sink has actually caught up, not just
    // once the raw bytes moved.
    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/file"))
                .query(&[("repo", "fixture"), ("path", "lib.rs")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let content_ok = body["content"]
                .as_str()
                .map(|c| c.contains("extra"))
                .unwrap_or(false);
            let highlights_ok = body["highlights"].as_array().is_some_and(|a| !a.is_empty());
            let symbols_ok = body["symbols"].as_array().is_some_and(|a| a.len() >= 2);
            content_ok && highlights_ok && symbols_ok
        }
    })
    .await;
    assert!(
        ok,
        "expected /api/file to serve the edited content with derived symbols+highlights"
    );

    let body: serde_json::Value = client
        .get(format!("{base}/api/file"))
        .query(&[("repo", "fixture"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["encoding"], "utf8");
    assert_eq!(body["lang"], "rust");
    assert!(
        body["highlights"].as_array().is_some_and(|a| !a.is_empty()),
        "expected non-empty highlight spans: {body:?}"
    );
    assert!(
        body["symbols"].as_array().is_some_and(|a| a.len() >= 2),
        "expected add+extra symbols: {body:?}"
    );

    let sse_ok = wait_until_async(Duration::from_secs(10), || {
        let events_buf = events_buf.clone();
        async move {
            let b = events_buf.lock().await;
            b.contains("mirror.updated") && b.contains("lib.rs")
        }
    })
    .await;
    assert!(
        sse_ok,
        "expected an SSE mirror.updated frame naming lib.rs: {}",
        events_buf.lock().await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_moves_head_updates_repos_and_emits_head_moved_sse() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("f.txt"), b"base\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "base"]);
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("f.txt"), b"feature\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "feature commit"]);
    git(&dir, &["checkout", "-q", "main"]);

    let (_tmp, base, _task) = boot_with_repo(&dir, "fixture").await;
    let client = reqwest::Client::new();

    assert!(
        wait_until_async(Duration::from_secs(15), || {
            let client = client.clone();
            let base = base.clone();
            async move {
                let body: serde_json::Value = client
                    .get(format!("{base}/api/repos"))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                body["repos"][0]["head"]["branch"] == "main"
            }
        })
        .await
    );

    let events_buf = open_events_buffer(&base).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    events_buf.lock().await.clear();

    git(&dir, &["checkout", "-q", "feature"]);

    let ok = wait_until_async(Duration::from_secs(20), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/repos"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["repos"][0]["head"]["branch"] == "feature"
        }
    })
    .await;
    assert!(ok, "expected /api/repos to report the new HEAD branch");

    let sse_ok = wait_until_async(Duration::from_secs(10), || {
        let events_buf = events_buf.clone();
        async move { events_buf.lock().await.contains("repo.head_moved") }
    })
    .await;
    assert!(
        sse_ok,
        "expected an SSE repo.head_moved frame: {}",
        events_buf.lock().await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tree_at_an_older_ref_differs_from_head() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"a\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    std::fs::write(dir.join("b.txt"), b"b\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c2 adds b.txt"]);

    let (_tmp, base, _task) = boot_with_repo(&dir, "fixture").await;
    let client = reqwest::Client::new();

    let head: serde_json::Value = client
        .get(format!("{base}/api/tree"))
        .query(&[("repo", "fixture"), ("ref", "HEAD")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let head_names: Vec<String> = head["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    assert!(head_names.contains(&"a.txt".to_string()));
    assert!(head_names.contains(&"b.txt".to_string()));

    let prev: serde_json::Value = client
        .get(format!("{base}/api/tree"))
        .query(&[("repo", "fixture"), ("ref", "HEAD~1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let prev_names: Vec<String> = prev["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    assert!(prev_names.contains(&"a.txt".to_string()));
    assert!(
        !prev_names.contains(&"b.txt".to_string()),
        "HEAD~1 must not yet have b.txt: {prev_names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbols_query_finds_a_known_function_by_substring() {
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

    let (_tmp, base, _task) = boot_with_repo(&dir, "fixture").await;
    let client = reqwest::Client::new();

    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/symbols"))
                .query(&[("repo", "fixture"), ("q", "special_search")])
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["matches"].as_array().is_some_and(|a| !a.is_empty())
        }
    })
    .await;
    assert!(
        ok,
        "expected the initial index walk to make the symbol findable"
    );

    let body: serde_json::Value = client
        .get(format!("{base}/api/symbols"))
        .query(&[("repo", "fixture"), ("q", "special_search")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let matches = body["matches"].as_array().unwrap();
    assert!(matches
        .iter()
        .any(|m| m["name"] == "special_search_target" && m["path"] == "search.rs"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleted_file_removes_files_row_but_tree_stays_odb_correct() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("doomed.txt"), b"bye\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repo(&dir, "fixture").await;
    let client = reqwest::Client::new();

    // Wait for the initial walk to index it (so /api/file's no-ref read
    // definitely starts from a state where the file IS visible).
    assert!(
        wait_until_async(Duration::from_secs(15), || {
            let client = client.clone();
            let base = base.clone();
            async move {
                client
                    .get(format!("{base}/api/file"))
                    .query(&[("repo", "fixture"), ("path", "doomed.txt")])
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .is_success()
            }
        })
        .await
    );

    std::fs::remove_file(dir.join("doomed.txt")).unwrap();

    // The `files` row itself is gone — observable over HTTP via
    // `/api/repos`'s `file_count` (a `COUNT(*)` straight off the store,
    // see `routes::repos`), which is what actually proves the sink's
    // `remove_path` → `store::delete_file` path ran, rather than merely
    // that the file is absent from disk (which `/api/file`'s no-ref read
    // would also report on its own, even if the sink never fired at all —
    // see that route's doc: it reads `fs::read` directly, not the store).
    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/repos"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["repos"][0]["file_count"].as_u64().unwrap_or(1) == 0
        }
    })
    .await;
    assert!(
        ok,
        "expected the sink's remove_path to delete the files row (file_count -> 0)"
    );

    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            client
                .get(format!("{base}/api/file"))
                .query(&[("repo", "fixture"), ("path", "doomed.txt")])
                .send()
                .await
                .unwrap()
                .status()
                == reqwest::StatusCode::NOT_FOUND
        }
    })
    .await;
    assert!(
        ok,
        "expected /api/file (no ref) to 404 once the file is gone"
    );

    // The git ODB still has it at HEAD — an uncommitted working-tree delete
    // doesn't change what's committed, so /api/tree correctly still lists
    // it (this is NOT a bug — see the module's own doc on `tree` being a
    // pure ODB read).
    let tree: serde_json::Value = client
        .get(format!("{base}/api/tree"))
        .query(&[("repo", "fixture"), ("ref", "HEAD")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<String> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"doomed.txt".to_string()),
        "the committed tree must still list doomed.txt: {names:?}"
    );
}

/// W2.2 — a mixed-language fixture repo (all six v1 languages: Rust,
/// Python, Ruby, TypeScript/JavaScript, Bash, YAML) indexes cleanly end to
/// end through the real boot walk, and `/api/symbols?q=` finds a symbol in
/// each of the two NEW-this-wave query styles: a TypeScript `tags.scm`
/// definition (an `interface`) and a YAML CST-walk key path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingest_e2e_mixed_language_repo_indexes_all_six_languages() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(
        dir.join("lib.rs"),
        b"fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("app.py"), b"def hi():\n    pass\n").unwrap();
    std::fs::write(dir.join("model.rb"), b"def area\n  1\nend\n").unwrap();
    std::fs::write(
        dir.join("app.ts"),
        b"interface Widget {\n  render(): void;\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("app.js"), b"function greet() {\n  return 1;\n}\n").unwrap();
    std::fs::write(dir.join("deploy.sh"), b"deploy() {\n  echo hi\n}\n").unwrap();
    std::fs::write(dir.join("config.yaml"), b"spec:\n  replicas: 3\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base, _task) = boot_with_repo(&dir, "fixture").await;
    let client = reqwest::Client::new();

    let ok = wait_until_async(Duration::from_secs(15), || {
        let client = client.clone();
        let base = base.clone();
        async move {
            let body: serde_json::Value = client
                .get(format!("{base}/api/repos"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            body["repos"][0]["file_count"].as_u64().unwrap_or(0) >= 7
        }
    })
    .await;
    assert!(ok, "expected the initial boot walk to index all 7 files");

    let body: serde_json::Value = client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repo = &body["repos"][0];
    for (path, lang) in [
        ("lib.rs", "rust"),
        ("app.py", "python"),
        ("model.rb", "ruby"),
        ("app.ts", "typescript"),
        ("app.js", "javascript"),
        ("deploy.sh", "bash"),
        ("config.yaml", "yaml"),
    ] {
        let file: serde_json::Value = client
            .get(format!("{base}/api/file"))
            .query(&[("repo", "fixture"), ("path", path)])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(file["lang"], lang, "path {path}: {file:?}");
    }
    assert!(repo["symbol_count"].as_u64().unwrap_or(0) >= 3);

    // A TypeScript `tags.scm` definition (interface).
    let ts_hit: serde_json::Value = client
        .get(format!("{base}/api/symbols"))
        .query(&[("repo", "fixture"), ("q", "Widget")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ts_matches = ts_hit["matches"].as_array().unwrap();
    assert!(
        ts_matches
            .iter()
            .any(|m| m["name"] == "Widget" && m["kind"] == "interface" && m["path"] == "app.ts"),
        "expected the TypeScript interface Widget: {ts_matches:?}"
    );

    // A YAML CST-walk key path.
    let yaml_hit: serde_json::Value = client
        .get(format!("{base}/api/symbols"))
        .query(&[("repo", "fixture"), ("q", "spec")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let yaml_matches = yaml_hit["matches"].as_array().unwrap();
    assert!(
        yaml_matches
            .iter()
            .any(|m| m["name"] == "spec" && m["kind"] == "key" && m["path"] == "config.yaml"),
        "expected the YAML key path spec: {yaml_matches:?}"
    );
}
