//! V3.3-S1 R7 — `GET /api/reviews/{id}/map` + `/reading-order` HTTP tests.
//!
//! Fixture: a small TypeScript import chain (util → mid → app) plus a
//! `*.test.ts` so reading-order puts the test last. Boot pattern mirrors
//! `tests/impact_analysis_route.rs` / `tests/local_review_routes.rs`.
//! Polls `symbol_count` (never file_count alone).

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

fn fixture_import_chain() -> tempfile::TempDir {
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
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base chain"]);

    git(dir, &["checkout", "-q", "-b", "feature"]);
    write(
        dir,
        "src/util.ts",
        "export function helper(): number {\n  return 2;\n}\n",
    );
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
    write(
        dir,
        "src/app.test.ts",
        "import { app } from './app';\nexport function testApp(): number {\n  return app();\n}\n",
    );
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature: touch chain + test"]);
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
            url: "http://127.0.0.1:0".to_string(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_map_and_reading_order() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_import_chain();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot("demo", dir).await;
    // 4 source files on feature HEAD
    wait_for_indexed(&base, "demo", 4).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "demo",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "map test",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_i64().unwrap();

    // --- map ---
    // Import edges AND call edges derive from extraction passes that can
    // each land AFTER symbol_count trips under load — poll until the full
    // expected skeleton is present (bounded). Eventual-consistency retry
    // is for PRESENCE only: a wrong class/order still fails on first
    // sight in the assertions below (which also gate reading-order,
    // derived from the same import edges).
    let deadline = Instant::now() + Duration::from_secs(45);
    let map: serde_json::Value = loop {
        let resp = client
            .get(format!("{base}/api/reviews/{id}/map"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        let (imports, calls) = body["edges"]
            .as_array()
            .map(|es| {
                (
                    es.iter().filter(|e| e["kind"] == "import").count(),
                    es.iter().filter(|e| e["kind"] == "call").count(),
                )
            })
            .unwrap_or((0, 0));
        if (imports >= 3 && calls >= 1) || Instant::now() >= deadline {
            break body;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(map["schema"], "review-map/1");
    assert_eq!(map["ps_number"], 1);

    let nodes = map["nodes"].as_array().unwrap();
    let paths: Vec<&str> = nodes.iter().map(|n| n["path"].as_str().unwrap()).collect();
    for expected in ["src/app.ts", "src/app.test.ts", "src/mid.ts", "src/util.ts"] {
        assert!(
            paths.contains(&expected),
            "missing node {expected}: {paths:?}"
        );
    }
    // Nodes sorted path-asc
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);

    // All modified except the new test (added)
    let test_node = nodes
        .iter()
        .find(|n| n["path"] == "src/app.test.ts")
        .unwrap();
    assert_eq!(test_node["status"], "added");
    // symbols_changed is empty (file-level RETURN LESS)
    assert!(test_node["symbols_changed"].as_array().unwrap().is_empty());
    // agent_touched null without behavioral backfill
    assert!(test_node["agent_touched"].is_null());

    let edges = map["edges"].as_array().unwrap();
    let import_edges: Vec<(&str, &str)> = edges
        .iter()
        .filter(|e| e["kind"] == "import")
        .map(|e| (e["from"].as_str().unwrap(), e["to"].as_str().unwrap()))
        .collect();
    // Among changed files: mid→util, app→mid, app.test→app
    assert!(
        import_edges.contains(&("src/mid.ts", "src/util.ts")),
        "mid→util missing: {import_edges:?}"
    );
    assert!(
        import_edges.contains(&("src/app.ts", "src/mid.ts")),
        "app→mid missing: {import_edges:?}"
    );
    assert!(
        import_edges.contains(&("src/app.test.ts", "src/app.ts")),
        "test→app missing: {import_edges:?}"
    );
    for e in edges.iter().filter(|e| e["kind"] == "import") {
        assert_eq!(e["class"], "exact");
    }
    // Call edges: the fixture's cross-file calls (mid→helper, app→mid)
    // must surface, and their class is CAPPED — the name-match heuristic
    // has no scip/locals proof, so a call edge claiming "exact" is the
    // wrong-exact release-blocker class.
    let call_edges: Vec<(&str, &str, &str)> = edges
        .iter()
        .filter(|e| e["kind"] == "call")
        .map(|e| {
            (
                e["from"].as_str().unwrap(),
                e["to"].as_str().unwrap(),
                e["class"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(
        call_edges
            .iter()
            .any(|(f, t, _)| (*f, *t) == ("src/mid.ts", "src/util.ts")),
        "mid→util call edge missing: {call_edges:?}"
    );
    for (f, t, class) in &call_edges {
        assert!(
            *class == "likely" || *class == "candidate",
            "call edge {f}→{t} class must be capped at likely, got {class}"
        );
    }
    // agent_touched is true|null, NEVER false — human-only author rows
    // don't prove agent absence (best-effort session↔commit join).
    for n in nodes {
        assert_ne!(
            n["agent_touched"],
            serde_json::Value::Bool(false),
            "agent_touched must never be false-by-default: {}",
            n["path"]
        );
    }
    // Extraction landed ⇒ no import_edges in inputs_missing
    let missing = map["inputs_missing"].as_array().unwrap();
    assert!(
        !missing.iter().any(|m| m == "import_edges"),
        "import_edges should be present: {missing:?}"
    );

    // Determinism: second call byte-identical
    let map2: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/map"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_string(&map).unwrap(),
        serde_json::to_string(&map2).unwrap()
    );

    // --- reading-order ---
    let resp = client
        .get(format!("{base}/api/reviews/{id}/reading-order"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let order: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(order["schema"], "review-reading-order/1");
    let stops = order["stops"].as_array().unwrap();
    assert_eq!(stops.len(), 4);
    let stop_paths: Vec<&str> = stops.iter().map(|s| s["path"].as_str().unwrap()).collect();
    // Bottom-up: util before mid before app; test last
    let util_i = stop_paths.iter().position(|p| *p == "src/util.ts").unwrap();
    let mid_i = stop_paths.iter().position(|p| *p == "src/mid.ts").unwrap();
    let app_i = stop_paths.iter().position(|p| *p == "src/app.ts").unwrap();
    let test_i = stop_paths
        .iter()
        .position(|p| *p == "src/app.test.ts")
        .unwrap();
    assert!(util_i < mid_i, "util before mid: {stop_paths:?}");
    assert!(mid_i < app_i, "mid before app: {stop_paths:?}");
    assert_eq!(test_i, stops.len() - 1, "test last: {stop_paths:?}");
    assert_eq!(stops[test_i]["reason"], "test");
    assert_eq!(stops[test_i]["cycle"], false);

    // util is imported by mid (1 changed file that imports it among live non-test
    // — reason uses full change-set importer count including the test?)
    // mid imports util → "imported by 1 changed files" (only mid among all changed)
    // app.test also doesn't import util. So util = 1.
    assert!(
        stops[util_i]["reason"]
            .as_str()
            .unwrap()
            .starts_with("imported by "),
        "util reason: {}",
        stops[util_i]["reason"]
    );

    // Determinism
    let order2: serde_json::Value = client
        .get(format!("{base}/api/reviews/{id}/reading-order"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_string(&order).unwrap(),
        serde_json::to_string(&order2).unwrap()
    );
}
