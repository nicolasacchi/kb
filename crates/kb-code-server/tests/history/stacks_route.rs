//! V3.3-S2 — end-to-end HTTP tests for `GET /api/stacks` and
//! `GET /api/stacks/layer-diff`. Modeled on `tests/time_routes.rs`: real
//! daemon via `serve_on_random_port_with_paths`, real git fixture, pure
//! git reads (no `wait_for_indexed` — stacks never touch the symbol
//! index).

use crate::common::git;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

fn commit(dir: &Path, file: &str, contents: &str, msg: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
        token_file: None,
        public_url: None,
    }
}

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

/// D ← A ← B chain: main base commit, A adds a.txt, B adds b.txt.
fn stack_fixture(dir: &Path) {
    init_repo(dir);
    commit(dir, "base.txt", "base\n", "base");
    git(dir, &["checkout", "-q", "-b", "A"]);
    commit(dir, "a.txt", "a\n", "A one");
    git(dir, &["checkout", "-q", "-b", "B"]);
    commit(dir, "b.txt", "b\n", "B one");
    git(dir, &["checkout", "-q", "main"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stacks_detects_two_layer_chain_with_per_layer_ahead() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    stack_fixture(&dir);

    // Pure git — no index wait needed (mirrors compare/branches tests).
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/stacks"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["schema"], "stacks/1");
    assert_eq!(body["default_branch"], "main");
    let stacks = body["stacks"].as_array().unwrap();
    assert_eq!(stacks.len(), 1, "one multi-layer stack: {body}");
    let layers = stacks[0]["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0]["branch"], "A");
    assert_eq!(layers[0]["base"], "main");
    assert_eq!(layers[0]["ahead"], 1);
    assert_eq!(layers[0]["stale"], false);
    assert_eq!(layers[0]["unresolved"], false);
    assert_eq!(layers[1]["branch"], "B");
    assert_eq!(layers[1]["base"], "A");
    assert_eq!(layers[1]["ahead"], 1);
    assert!(layers[0]["tip"]["sha"].as_str().unwrap().len() >= 40);
    assert_eq!(layers[0]["tip"]["subject"], "A one");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stacks_excludes_single_layer_by_default_includes_with_all() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    commit(&dir, "base.txt", "base\n", "base");
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    commit(&dir, "f.txt", "f\n", "feature one");
    git(&dir, &["checkout", "-q", "main"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let body: serde_json::Value = client
        .get(format!("{base}/api/stacks"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["stacks"].as_array().unwrap().len(),
        0,
        "single-layer excluded by default: {body}"
    );

    let body_all: serde_json::Value = client
        .get(format!("{base}/api/stacks"))
        .query(&[("repo", "fixture"), ("all", "true")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let stacks = body_all["stacks"].as_array().unwrap();
    assert_eq!(stacks.len(), 1);
    assert_eq!(stacks[0]["layers"][0]["branch"], "feature");
    assert_eq!(stacks[0]["layers"][0]["base"], "main");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layer_diff_of_b_shows_only_b_files_not_a() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    stack_fixture(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/stacks/layer-diff"))
        .query(&[("repo", "fixture"), ("branch", "B")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["schema"], "stacks-layer-diff/1");
    assert_eq!(body["branch"], "B");
    assert_eq!(body["base"], "A");
    assert_eq!(body["stale"], false);
    assert!(body["base_tip"].as_str().unwrap().len() >= 40);
    assert!(body["tip"].as_str().unwrap().len() >= 40);

    let paths: Vec<&str> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"b.txt"),
        "layer-diff of B must include b.txt: {paths:?}"
    );
    assert!(
        !paths.contains(&"a.txt"),
        "layer-diff of B must NOT include A's a.txt: {paths:?}"
    );
    assert!(
        !paths.contains(&"base.txt"),
        "layer-diff of B must NOT include base.txt: {paths:?}"
    );
    assert_eq!(body["totals"]["files"], paths.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stacks_flags_stale_layer_when_base_advances() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    stack_fixture(&dir);
    // Advance A after B was cut from it.
    git(&dir, &["checkout", "-q", "A"]);
    commit(&dir, "a2.txt", "a2\n", "A two after B cut");
    git(&dir, &["checkout", "-q", "main"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/stacks"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let layers = body["stacks"][0]["layers"].as_array().unwrap();
    let b = layers.iter().find(|l| l["branch"] == "B").expect("B layer");
    assert_eq!(b["base"], "A");
    assert_eq!(b["stale"], true, "B should be stale: {b}");

    // layer-diff still works and echoes stale.
    let diff: serde_json::Value = client
        .get(format!("{base}/api/stacks/layer-diff"))
        .query(&[("repo", "fixture"), ("branch", "B")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diff["stale"], true);
    assert_eq!(diff["base"], "A");
}
