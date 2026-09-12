//! PRR-F (design-ui.md §12.2, "Reviewer X-ray") — `GET
//! /api/reviews/{id}/impact?path=` HTTP tests.
//!
//! Fixture: a small TypeScript call chain — `outside.ts` and `app.ts` both
//! call `mid.ts`'s `mid()`, which itself calls `util.ts`'s `helper()`. The
//! feature branch touches `mid.ts` (inside `mid()`'s own body) AND
//! `app.ts` (inside `app()`'s own body), but leaves `outside.ts`
//! untouched — so `mid()`'s one caller in `app.ts` is "in this diff" and
//! its one caller in `outside.ts` is "out of this diff", the exact
//! blast-radius distinction the chip exists to show. Boot pattern mirrors
//! `review_map_route.rs`.

use crate::common::git;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn fixture_call_chain() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);

    write(
        dir,
        "src/util.ts",
        "export function helper(): number {\n  return 1;\n}\n",
    );
    write(
        dir,
        "src/mid.ts",
        "import { helper } from './util';\nexport function mid(): number {\n  return helper();\n}\n",
    );
    write(
        dir,
        "src/app.ts",
        "import { mid } from './mid';\nexport function app(): number {\n  return mid();\n}\n",
    );
    write(
        dir,
        "src/outside.ts",
        "import { mid } from './mid';\nexport function outsideCaller(): number {\n  return mid();\n}\n",
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base chain"]);

    git(dir, &["checkout", "-q", "-b", "feature"]);
    // Touch mid() itself AND app() — outside.ts stays untouched.
    write(
        dir,
        "src/mid.ts",
        "import { helper } from './util';\nexport function mid(): number {\n  return helper() + 1;\n}\n",
    );
    write(
        dir,
        "src/app.ts",
        "import { mid } from './mid';\nexport function app(): number {\n  return mid() * 2;\n}\n",
    );
    git(dir, &["add", "-A"]);
    git(
        dir,
        &["commit", "-q", "-m", "feature: bump mid() and app()"],
    );
    tmp
}

async fn boot(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        review: ReviewSection::default(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

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

/// Same poll as [`wait_for_indexed`] but for a fixture with NO extractable
/// symbols at all (a markdown-only repo) — waiting on `symbol_count > 0`
/// there would spin to the deadline every time.
async fn wait_for_indexed_files_only(base: &str, repo: &str, expected_files: usize) {
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
                    if count >= expected_files {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "index timeout waiting for file_count"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_impact_splits_callers_by_diff_membership() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_call_chain();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot("demo", dir).await;
    wait_for_indexed(&base, "demo", 4).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "demo",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "impact test",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_i64().unwrap();

    // Poll: call-site extraction can land after symbol_count trips.
    let deadline = Instant::now() + Duration::from_secs(45);
    let body: serde_json::Value = loop {
        let resp = client
            .get(format!("{base}/api/reviews/{id}/impact"))
            .query(&[("path", "src/mid.ts")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
        let body: serde_json::Value = resp.json().await.unwrap();
        let total = body["callers_total"].as_i64().unwrap_or(0);
        if total >= 2 || Instant::now() >= deadline {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    assert_eq!(body["schema"], "review-impact-file/1");
    assert_eq!(body["path"], "src/mid.ts");
    assert_eq!(body["lang_supported"], true);

    let symbols = body["changed_symbols"].as_array().unwrap();
    let mid_sym = symbols
        .iter()
        .find(|s| s["name"] == "mid")
        .unwrap_or_else(|| panic!("mid() must be a changed symbol: {symbols:?}"));
    assert_eq!(mid_sym["callers_total"], 2, "{body}");
    assert_eq!(
        mid_sym["callers_in_diff"], 1,
        "app.ts is also changed in this review: {body}"
    );
    assert_eq!(
        mid_sym["callers_out_of_diff"], 1,
        "outside.ts was never touched by this review: {body}"
    );

    assert_eq!(body["callers_total"], 2);
    assert_eq!(body["callers_in_diff"], 1);
    assert_eq!(body["callers_out_of_diff"], 1);
    assert_eq!(body["symbols_truncated"], false);

    // Determinism: second call byte-identical.
    let body2: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/impact"))
        .query(&[("path", "src/mid.ts")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_string(&body).unwrap(),
        serde_json::to_string(&body2).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_impact_400s_on_a_path_outside_the_change_set() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_call_chain();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot("demo", dir).await;
    wait_for_indexed(&base, "demo", 4).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "demo",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "impact scope test",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // outside.ts is untouched by the feature branch — not in the change set.
    let resp = client
        .get(format!("{base}/api/reviews/{id}/impact"))
        .query(&[("path", "src/outside.ts")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "{}", resp.text().await.unwrap());
}

/// A repo-config'd but caller-graph-UNSUPPORTED language (plain `.md` here
/// — no `lang::detect` hit at all) degrades to `lang_supported: false`
/// rather than a fabricated zero-callers claim.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_impact_degrades_honestly_for_an_unsupported_language() {
    let _guard = SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    write(dir, "README.md", "# Base\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    write(dir, "README.md", "# Base\n\nMore text.\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature: readme edit"]);

    let (_daemon_tmp, base) = boot("demo", dir).await;
    wait_for_indexed_files_only(&base, "demo", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "demo",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "unsupported lang test",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = client
        .get(format!("{base}/api/reviews/{id}/impact"))
        .query(&[("path", "README.md")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["lang_supported"], false);
    assert!(body["changed_symbols"].as_array().unwrap().is_empty());
    assert_eq!(body["callers_total"], 0);
    assert!(body["note"].as_str().unwrap().contains("rust"));
}
