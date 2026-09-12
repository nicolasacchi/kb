//! V3.3-Q1 — named deterministic recipes HTTP tests.
//!
//! Catalog is pure (no repo). Run path boots a temp git fixture, waits for
//! symbol_count (not just file_count — call_sites lag), then exercises
//! new-public-api, god-functions, and failure-tainted inputs_missing.

use crate::common::git;
use kb_code_server::config::{
    BehavioralSection, KbCodeConfig, KbDaemonSection, RepoEntry, ScopesSection, SemanticSection,
};
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

fn commit_as(
    dir: &Path,
    email: &str,
    name: &str,
    message: &str,
    files: &[(&str, &str)],
    date: &str,
) {
    git(dir, &["config", "user.email", email]);
    git(dir, &["config", "user.name", name]);
    for (rel, contents) in files {
        write_file(dir, rel, contents);
    }
    git(dir, &["add", "-A"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("git commit");
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Fixture with:
/// - lib.rs: private + public fns; public helper_new introduced late
/// - caller.rs: calls helper (fan edges for god-functions)
/// - grow.rs: complexity growth across commits
fn build_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    init_repo(&dir);

    // c1: early — private + small public
    commit_as(
        &dir,
        "alice@example.com",
        "Alice",
        "c1: baseline",
        &[
            (
                "lib.rs",
                "fn private_helper() {}\n\
                 pub fn old_api() { private_helper(); }\n",
            ),
            ("grow.rs", "fn g() {}\n"),
            ("caller.rs", "fn use_old() { old_api(); }\n"),
        ],
        "2024-01-01T12:00:00 +0000",
    );
    // c2: late public API + more complex grow.rs + more calls
    commit_as(
        &dir,
        "alice@example.com",
        "Alice",
        "c2: new public + growth",
        &[
            (
                "lib.rs",
                "fn private_helper() {}\n\
                 pub fn old_api() { private_helper(); }\n\
                 pub fn helper_new() {\n\
                     private_helper();\n\
                     private_helper();\n\
                     private_helper();\n\
                 }\n\
                 pub fn godlike() {\n\
                     helper_new();\n\
                     helper_new();\n\
                     old_api();\n\
                     private_helper();\n\
                     private_helper();\n\
                     private_helper();\n\
                     private_helper();\n\
                 }\n",
            ),
            (
                "grow.rs",
                "fn g() {\n\
                     let a = 1;\n\
                         let b = 2;\n\
                             let c = 3;\n\
                                 let d = 4;\n\
                                     let e = 5;\n\
                 }\n",
            ),
            (
                "caller.rs",
                "fn use_old() { old_api(); }\n\
                 fn use_new() { helper_new(); godlike(); }\n",
            ),
        ],
        "2024-06-15T12:00:00 +0000",
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
    scopes.insert("tests".to_string(), vec!["**/tests/**".to_string()]);

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
            window_days: 3650,
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

/// Wait on file_count AND symbol_count — never file_count alone (flakes
/// under load when call_sites / symbols lag).
async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
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
                        // Extra beat so call_sites land after symbols.
                        tokio::time::sleep(Duration::from_millis(200)).await;
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

#[tokio::test]
async fn catalog_and_recipes_run() {
    let (_repo_tmp, repo_dir) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 3).await;

    let client = reqwest::Client::new();

    // --- catalog: pure, no repo ---
    let resp = client
        .get(format!("{}/api/recipes", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let cat: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(cat["schema"], "recipes/1");
    let recipes = cat["recipes"].as_array().unwrap();
    assert_eq!(recipes.len(), 6);
    let names: Vec<&str> = recipes.iter().filter_map(|r| r["name"].as_str()).collect();
    assert!(names.contains(&"new-public-api"));
    assert!(names.contains(&"god-functions"));
    assert!(names.contains(&"failure-tainted"));
    // since required on new-public-api
    let npa = recipes
        .iter()
        .find(|r| r["name"] == "new-public-api")
        .unwrap();
    assert!(npa["params"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "since" && p["required"] == true));

    // --- unknown name → 404 listing catalog ---
    let resp = client
        .get(format!("{}/api/recipes/nope", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let err: serde_json::Value = resp.json().await.unwrap();
    let msg = err["error"].as_str().unwrap_or("");
    assert!(msg.contains("new-public-api"), "{msg}");

    // --- missing required since → 400 ---
    let resp = client
        .get(format!("{}/api/recipes/new-public-api", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let err: serde_json::Value = resp.json().await.unwrap();
    assert!(
        err["error"].as_str().unwrap_or("").contains("since"),
        "{err}"
    );

    // --- new-public-api: since before c2 → helper_new / godlike / old_api? ---
    // since=2024-03-01 is after c1 (Jan) and before c2 (Jun).
    // old_api was introduced in c1 → excluded; helper_new + godlike in c2 → included.
    let resp = client
        .get(format!("{}/api/recipes/new-public-api", boot.base))
        .query(&[("repo", "demo"), ("since", "2024-03-01")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "new-public-api status");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["recipe"], "new-public-api");
    assert_eq!(body["recipe_version"], 1);
    assert!(body["inputs_missing"].as_array().unwrap().is_empty());
    let items = body["items"].as_array().unwrap();
    let syms: Vec<&str> = items.iter().filter_map(|i| i["symbol"].as_str()).collect();
    assert!(
        syms.contains(&"helper_new") || syms.contains(&"godlike"),
        "expected late public symbols, got {syms:?} full={body}"
    );
    assert!(
        !syms.contains(&"private_helper"),
        "private must not appear: {syms:?}"
    );
    for it in items {
        assert_eq!(it["class"], "exact");
        assert!(it["first_seen"]["commit"].as_str().unwrap().len() >= 7);
    }

    // Determinism: run twice ⇒ identical
    let resp2 = client
        .get(format!("{}/api/recipes/new-public-api", boot.base))
        .query(&[("repo", "demo"), ("since", "2024-03-01")])
        .send()
        .await
        .unwrap();
    let body2: serde_json::Value = resp2.json().await.unwrap();
    assert_eq!(body["items"], body2["items"]);

    // --- god-functions: non-empty, score terms present ---
    let resp = client
        .get(format!("{}/api/recipes/god-functions", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["recipe"], "god-functions");
    let items = body["items"].as_array().unwrap();
    assert!(!items.is_empty(), "god-functions empty: {body}");
    let top = &items[0];
    assert!(top["terms"]["line_span"].as_u64().unwrap() >= 1);
    assert!(top.get("class").is_some());
    // Scores non-increasing
    let scores: Vec<u64> = items
        .iter()
        .map(|i| i["score"].as_u64().unwrap_or(0))
        .collect();
    for w in scores.windows(2) {
        assert!(w[0] >= w[1], "scores not desc: {scores:?}");
    }

    // --- failure-tainted: no session_signals ⇒ inputs_missing ---
    let resp = client
        .get(format!("{}/api/recipes/failure-tainted", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["recipe"], "failure-tainted");
    let missing = body["inputs_missing"].as_array().unwrap();
    assert!(
        missing.iter().any(|m| m == "session_signals"),
        "expected session_signals in inputs_missing: {body}"
    );
    assert!(
        body["items"].as_array().unwrap().is_empty(),
        "items must be empty when inputs missing: {body}"
    );

    // --- agent-only: no agent data ⇒ inputs_missing ---
    let resp = client
        .get(format!("{}/api/recipes/agent-only-symbols", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let missing = body["inputs_missing"].as_array().unwrap();
    assert!(missing.iter().any(|m| m == "agent_attribution"), "{body}");
    assert!(body["items"].as_array().unwrap().is_empty());

    // --- unreviewed-hotspots pre-explicit-backfill: the mirror ALSO
    // ingests behavioral counters automatically on head-move (v3.2 B1),
    // so "no behavioral rows yet" is a RACE, not a stageable state.
    // Honest pin: either state is valid, but never the dishonest mix
    // (items populated while claiming behavioral is missing, or an
    // empty result indistinguishable from missing input).
    let resp = client
        .get(format!("{}/api/recipes/unreviewed-hotspots", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let missing = body["inputs_missing"].as_array().unwrap();
    let items_empty = body["items"].as_array().unwrap().is_empty();
    let behavioral_missing = missing.iter().any(|m| m == "behavioral");
    assert!(
        (behavioral_missing && items_empty) || (!behavioral_missing && !items_empty),
        "inputs_missing and items must agree (auto-ingest race): {body}"
    );

    // After backfill, unreviewed-hotspots returns items (no reviews → all hot).
    let resp = client
        .post(format!("{}/api/behavioral/backfill", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = client
        .get(format!("{}/api/recipes/unreviewed-hotspots", boot.base))
        .query(&[("repo", "demo")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["inputs_missing"].as_array().unwrap().is_empty());
    assert!(
        body["note"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("unreviewed")
            || body["note"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .contains("no local reviews"),
        "note should mention unreviewed/no reviews: {body}"
    );
    assert!(!body["items"].as_array().unwrap().is_empty());

    // --- complexity-climbers: grow.rs should rise ---
    let resp = client
        .get(format!("{}/api/recipes/complexity-climbers", boot.base))
        .query(&[("repo", "demo"), ("since", "HEAD~1")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "complexity-climbers");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["recipe"], "complexity-climbers");
    let items = body["items"].as_array().unwrap();
    let paths: Vec<&str> = items.iter().filter_map(|i| i["path"].as_str()).collect();
    assert!(
        paths.iter().any(|p| p.contains("grow")),
        "expected grow.rs in climbers: {paths:?} {body}"
    );
    for it in items {
        let t = &it["terms"];
        assert!(t["delta"].as_i64().unwrap() > 0);
        assert!(t["loc_now"].as_u64().unwrap() >= t["loc_then"].as_u64().unwrap());
    }
}
