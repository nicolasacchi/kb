//! Phase N — `GET /api/todos` filtering (marker, path_prefix, scope
//! include/exclude) + `GET /api/scopes`. Boots a daemon the same way
//! `reading_sets_route.rs` does.
//!
//! V72-J1: this file is UNCHANGED on purpose. `/api/todos` is now a
//! filtered view over the `comments/1` index (`todo_items` and
//! `extract::extract_todos` are gone), and this suite passing verbatim is
//! the byte-compatibility proof for that subsumption. The two documented
//! ROW-SET changes — outline-tier languages are now scanned, and a
//! two-marker line reports the leftmost keyword — are asserted in
//! `comments_route.rs`, since neither shape exists in this fixture.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ScopesSection};
use kb_core::paths::KbPaths;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    // Nested under a package dir so the path is unambiguously
    // `**/tests/**` (and avoids any host-level ignore of a top-level
    // `tests/` directory some operators keep in their global gitignore).
    std::fs::create_dir_all(dir.join("pkg/tests")).unwrap();
    std::fs::create_dir_all(dir.join("dist")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "// TODO: core work\n// FIXME: later\nfn a() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("pkg/tests/it.rs"), "// TODO: test me\nfn t() {}\n").unwrap();
    std::fs::write(
        dir.join("dist/out.rs"),
        "// TODO: generated should be scopable\nfn g() {}\n",
    )
    .unwrap();
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

async fn boot(path: &Path) -> Boot {
    let mut scopes = BTreeMap::new();
    scopes.insert(
        "generated".to_string(),
        vec!["**/dist/**".to_string(), "**/*.lock".to_string()],
    );
    scopes.insert(
        "tests".to_string(),
        vec!["**/tests/**".to_string(), "**/*.test.*".to_string()],
    );
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "r".to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        scopes: ScopesSection { map: scopes },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

async fn wait_for_indexed(base: &str, expected_files: usize, expected_todos: u64) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == "r"))
                {
                    if entry["file_count"].as_u64().unwrap_or(0) as usize >= expected_files {
                        // Also wait until every fixture file's TODOs landed —
                        // file_count alone races replace_todo_items per file
                        // (the file row lands before its derived todos), and
                        // "at least one todo" still under-waits the walk's
                        // remaining files (seen flaking the 3-file fixture on
                        // the integrated gate, 2026-07-31).
                        if let Ok(todos) = client
                            .get(format!("{base}/api/todos"))
                            .query(&[("repo", "r")])
                            .send()
                            .await
                        {
                            if let Ok(tbody) = todos.json::<serde_json::Value>().await {
                                if tbody["total"].as_u64().unwrap_or(0) >= expected_todos {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(Instant::now() < deadline, "indexing timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn get_todos(base: &str, query: &[(&str, &str)]) -> serde_json::Value {
    let client = reqwest::Client::new();
    client
        .get(format!("{base}/api/todos"))
        .query(query)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todos_list_and_marker_path_prefix_filters() {
    let repo_tmp = fixture_repo();
    let boot = boot(repo_tmp.path()).await;
    wait_for_indexed(&boot.base, 3, 4).await;

    let all = get_todos(&boot.base, &[("repo", "r")]).await;
    let items = all["items"].as_array().unwrap();
    assert!(
        items.len() >= 3,
        "expected at least TODO/FIXME across files: {all}"
    );
    assert_eq!(all["total"].as_u64().unwrap() as usize, items.len());
    assert_eq!(all["truncated"], false);

    let only_todo = get_todos(&boot.base, &[("repo", "r"), ("marker", "TODO")]).await;
    let markers: Vec<&str> = only_todo["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["marker"].as_str().unwrap())
        .collect();
    assert!(!markers.is_empty());
    assert!(markers.iter().all(|m| *m == "TODO"), "{only_todo}");

    let src_only = get_todos(&boot.base, &[("repo", "r"), ("path_prefix", "src/")]).await;
    for item in src_only["items"].as_array().unwrap() {
        assert!(item["path"].as_str().unwrap().starts_with("src/"), "{item}");
    }
    assert!(!src_only["items"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn todos_scope_include_and_exclude() {
    let repo_tmp = fixture_repo();
    let boot = boot(repo_tmp.path()).await;
    wait_for_indexed(&boot.base, 3, 4).await;

    let all = get_todos(&boot.base, &[("repo", "r")]).await;
    assert!(
        all["total"].as_u64().unwrap_or(0) > 0,
        "expected todos after index: {all}"
    );
    // Sanity: at least one path under tests/ must have been extracted, else
    // the scope filter has nothing to include.
    let all_paths: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["path"].as_str())
        .collect();
    assert!(
        all_paths
            .iter()
            .any(|p| p.contains("/tests/") || p.starts_with("tests/")),
        "fixture must produce a tests/ todo: {all_paths:?}"
    );

    let tests_only = get_todos(&boot.base, &[("repo", "r"), ("scope", "tests")]).await;
    let paths: Vec<&str> = tests_only["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap())
        .collect();
    assert!(
        !paths.is_empty(),
        "scope=tests empty; all was {all}; got {tests_only}"
    );
    assert!(
        paths
            .iter()
            .all(|p| p.contains("/tests/") || p.starts_with("tests/")),
        "{paths:?}"
    );

    let no_generated = get_todos(&boot.base, &[("repo", "r"), ("scope", "!generated")]).await;
    for item in no_generated["items"].as_array().unwrap() {
        assert!(
            !item["path"].as_str().unwrap().starts_with("dist/"),
            "excluded generated: {item}"
        );
    }

    let unknown = reqwest::Client::new()
        .get(format!("{}/api/todos", boot.base))
        .query(&[("repo", "r"), ("scope", "nope")])
        .send()
        .await
        .unwrap();
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scopes_endpoint_returns_configured_map() {
    let repo_tmp = fixture_repo();
    let boot = boot(repo_tmp.path()).await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/api/scopes", boot.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["scopes"]["generated"].is_array());
    assert!(body["scopes"]["tests"].is_array());
}
