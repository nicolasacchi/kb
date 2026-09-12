//! V3.2-B1 — behavioral store HTTP tests.
//!
//! Fixture: scripted multi-author history with a hot path, a always-co-
//! changed pair, and skewed ownership. Asserts exact counter values after
//! backfill, coupling confidence math, ownership shares + entropy, scope
//! filter, caps/truncation, and incremental advance without full rebuild.

use crate::common::git;
use kb_code_server::config::{
    BehavioralSection, KbCodeConfig, KbDaemonSection, RepoEntry, ScopesSection, SemanticSection,
};
use kb_code_server::store::Store;
use kb_core::paths::KbPaths;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "alice@example.com"]);
    git(dir, &["config", "user.name", "Alice"]);
}

fn write_file(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn commit_as(dir: &Path, email: &str, name: &str, message: &str, files: &[(&str, &str)]) {
    git(dir, &["config", "user.email", email]);
    git(dir, &["config", "user.name", name]);
    for (rel, contents) in files {
        write_file(dir, rel, contents);
    }
    git(dir, &["add", "-A"]);
    // Fixed dates so counters/timestamps are deterministic.
    let env_date = "2024-01-15T12:00:00 +0000";
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_DATE", env_date)
        .env("GIT_COMMITTER_DATE", env_date)
        .output()
        .expect("git commit");
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Scripted history:
/// - hot.rs touched 5 times (alice×4, bob×1)
/// - pair_a.rs + pair_b.rs always co-changed (3 commits, alice)
/// - cold.rs touched once (bob)
/// - tests/t.rs once (alice) for scope filter
fn build_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    init_repo(&dir);

    commit_as(
        &dir,
        "alice@example.com",
        "Alice",
        "c1: hot + pair",
        &[
            ("hot.rs", "fn a() {}\n"),
            ("pair_a.rs", "fn pa() {}\n"),
            ("pair_b.rs", "fn pb() {}\n"),
        ],
    );
    commit_as(
        &dir,
        "alice@example.com",
        "Alice",
        "c2: hot + pair",
        &[
            ("hot.rs", "fn a() { let x = 1; }\n"),
            ("pair_a.rs", "fn pa() { 1 }\n"),
            ("pair_b.rs", "fn pb() { 1 }\n"),
        ],
    );
    commit_as(
        &dir,
        "alice@example.com",
        "Alice",
        "c3: hot + pair",
        &[
            ("hot.rs", "fn a() { let x = 2; }\n"),
            ("pair_a.rs", "fn pa() { 2 }\n"),
            ("pair_b.rs", "fn pb() { 2 }\n"),
        ],
    );
    commit_as(
        &dir,
        "alice@example.com",
        "Alice",
        "c4: hot only",
        &[("hot.rs", "fn a() { let x = 3; }\n")],
    );
    commit_as(
        &dir,
        "bob@example.com",
        "Bob",
        "c5: hot + cold + tests",
        &[
            ("hot.rs", "fn a() { let x = 4; }\n"),
            ("cold.rs", "fn c() {}\n"),
            ("tests/t.rs", "#[test] fn t() {}\n"),
        ],
    );

    (tmp, dir)
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(repo_name: &str, repo_dir: &Path) -> Boot {
    let mut scopes = BTreeMap::new();
    scopes.insert(
        "tests".to_string(),
        vec!["**/tests/**".to_string(), "**/*.test.*".to_string()],
    );
    scopes.insert("generated".to_string(), vec!["**/dist/**".to_string()]);

    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        semantic: SemanticSection::default(),
        scopes: ScopesSection { map: scopes },
        behavioral: BehavioralSection {
            enabled: true,
            window_days: 3650, // wide so fixture dates always in-window
            max_commit_files: 30,
        },
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

/// Wait on file_count AND symbol_count — never file_count alone.
async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    let symbols = entry["symbol_count"].as_u64().unwrap_or(0);
                    if count >= expected_files && symbols > 0 {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "index timeout waiting for symbol_count"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn post_backfill(base: &str, repo: &str) -> serde_json::Value {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/behavioral/backfill"))
        .query(&[("repo", repo)])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "backfill status");
    resp.json().await.unwrap()
}

#[tokio::test]
async fn backfill_exact_counters_coupling_ownership_scope_caps() {
    let (_repo_tmp, repo_dir) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 4).await;

    let stats = post_backfill(&boot.base, "demo").await;
    assert_eq!(stats["schema"], "behavioral/1");
    assert_eq!(stats["commits"].as_u64().unwrap(), 5);
    assert!(stats["full_rebuild"].as_bool().unwrap());
    assert!(stats["last_commit_sha"].as_str().unwrap().len() >= 7);

    let client = reqwest::Client::new();

    // --- hotspots: hot.rs has most revisions ---
    let resp = client
        .get(format!("{}/api/behavioral/hotspots", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "behavioral/1");
    let items = body["items"].as_array().unwrap();
    assert!(!items.is_empty());
    let hot = items.iter().find(|i| i["path"] == "hot.rs").unwrap();
    assert_eq!(hot["revisions"].as_i64().unwrap(), 5);
    assert!(hot["hotspot"]["terms"]["churn_rank"].as_u64().is_some());
    assert!(hot["hotspot"]["score"].as_f64().unwrap() > 0.0);
    // Decomposed terms always present — never a bare grade.
    assert!(hot["hotspot"]["terms"]["complexity_rank"].is_number());
    assert!(hot["complexity"]["loc"].is_number());

    // --- scope exclude tests ---
    let resp = client
        .get(format!("{}/api/behavioral/hotspots", boot.base))
        .query(&[("repo", "demo"), ("scope", "!tests")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let paths: Vec<_> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["path"].as_str())
        .collect();
    assert!(!paths.iter().any(|p| p.starts_with("tests/")));

    // --- scope include tests ---
    let resp = client
        .get(format!("{}/api/behavioral/hotspots", boot.base))
        .query(&[("repo", "demo"), ("scope", "tests")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["path"], "tests/t.rs");

    // --- caps / truncation ---
    let resp = client
        .get(format!("{}/api/behavioral/hotspots", boot.base))
        .query(&[("repo", "demo"), ("limit", "1")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert!(body["truncated"].as_bool().unwrap());
    assert!(body["total"].as_u64().unwrap() > 1);

    // --- coupling: pair_a always with pair_b (3 co_commits); conf = 3/3 = 1.0 ---
    let resp = client
        .get(format!("{}/api/behavioral/coupling", boot.base))
        .query(&[("repo", "demo"), ("path", "pair_a.rs")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "behavioral/1");
    let partners = body["partners"].as_array().unwrap();
    let pb = partners
        .iter()
        .find(|p| p["path"] == "pair_b.rs")
        .expect("pair_b partner");
    assert_eq!(pb["co_commits"].as_i64().unwrap(), 3);
    assert_eq!(pb["support"].as_i64().unwrap(), 3);
    let conf = pb["confidence"].as_f64().unwrap();
    assert!((conf - 1.0).abs() < 1e-9, "conf={conf}");

    // --- ownership: hot.rs alice 4/5=0.8, bob 1/5=0.2 ---
    let resp = client
        .get(format!("{}/api/behavioral/ownership", boot.base))
        .query(&[("repo", "demo"), ("path", "hot.rs")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "behavioral/1");
    assert_eq!(body["total_commits"].as_i64().unwrap(), 5);
    assert!((body["ownership"].as_f64().unwrap() - 0.8).abs() < 1e-9);
    let authors = body["authors"].as_array().unwrap();
    assert_eq!(authors.len(), 2);
    assert_eq!(authors[0]["author"], "alice@example.com");
    assert_eq!(authors[0]["commits"].as_i64().unwrap(), 4);
    assert!((authors[0]["share"].as_f64().unwrap() - 0.8).abs() < 1e-9);
    assert_eq!(authors[1]["author"], "bob@example.com");
    assert_eq!(authors[1]["commits"].as_i64().unwrap(), 1);
    // both major (>5%); fragmentation = Shannon of [0.8, 0.2]
    assert_eq!(body["major"].as_u64().unwrap(), 2);
    assert_eq!(body["minor"].as_u64().unwrap(), 0);
    let h = body["fragmentation"].as_f64().unwrap();
    let expected = -(0.8_f64 * 0.8_f64.ln() + 0.2_f64 * 0.2_f64.ln());
    assert!((h - expected).abs() < 1e-9, "entropy {h} vs {expected}");

    // --- age: from blame, has lines + buckets ---
    let resp = client
        .get(format!("{}/api/behavioral/age", boot.base))
        .query(&[("repo", "demo"), ("path", "hot.rs")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "behavioral/1");
    assert!(body["lines"].as_u64().unwrap() >= 1);
    let buckets = body["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 5);
    let labels: Vec<_> = buckets.iter().filter_map(|b| b["label"].as_str()).collect();
    assert_eq!(labels, vec!["<7d", "<30d", "<90d", "<1y", "older"]);
}

#[test]
fn incremental_advances_without_full_rebuild() {
    // Library-path test (same functions the head_moved worker calls) so we
    // own the Store and can assert exact counter deltas without racing the
    // daemon's Mutex.
    let (_repo_tmp, repo_dir) = build_fixture();
    let db_tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&db_tmp.path().join("index.db")).unwrap();
    let root = std::fs::canonicalize(&repo_dir).unwrap();
    let repo_id = store.upsert_repo("demo", root.to_str().unwrap()).unwrap();
    let repo_entry = RepoEntry {
        name: "demo".into(),
        path: root,
    };
    let cfg = BehavioralSection {
        enabled: true,
        window_days: 3650,
        max_commit_files: 30,
    };

    let stats1 =
        kb_code_server::behavioral::backfill_repo(&repo_entry, repo_id, &cfg, &store).unwrap();
    assert!(stats1.full_rebuild);
    let last1 = stats1.last_commit_sha.clone().expect("head sha");
    let revs_before = store
        .path_stats_for(repo_id, "hot.rs")
        .unwrap()
        .expect("hot.rs stats")
        .revisions;
    assert_eq!(revs_before, 5);

    commit_as(
        &repo_dir,
        "alice@example.com",
        "Alice",
        "c6: hot again",
        &[("hot.rs", "fn a() { let x = 5; }\n")],
    );

    let stats2 =
        kb_code_server::behavioral::incremental_update(&repo_entry, repo_id, &cfg, &store).unwrap();
    assert!(
        !stats2.full_rebuild,
        "expected pure incremental, got full rebuild: {stats2:?}"
    );
    assert_eq!(stats2.commits, 1, "exactly one new commit: {stats2:?}");
    assert_ne!(
        stats2.last_commit_sha.as_deref(),
        Some(last1.as_str()),
        "last_commit_sha must move"
    );

    let revs_after = store
        .path_stats_for(repo_id, "hot.rs")
        .unwrap()
        .expect("hot.rs stats")
        .revisions;
    assert_eq!(revs_after, revs_before + 1);
}

#[tokio::test]
async fn unknown_scope_404s() {
    let (_repo_tmp, repo_dir) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 4).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/behavioral/hotspots", boot.base))
        .query(&[("repo", "demo"), ("scope", "nope")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
