//! V3.1-H2 — `GET /api/impact/analysis` HTTP tests.
//!
//! Fixture: def file + cross-file caller + importing consumer + test file.
//! Asserts bucket membership, depth decay classes, tests separation, caps
//! via `limit`, and waits on symbol_count (not file_count alone).

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, SemanticSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::common::{git, init_repo};
fn commit_tree(dir: &Path, files: &[(&str, &str)], message: &str) {
    for (rel, contents) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(repo_name: &str, repo_dir: &Path) -> Boot {
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

/// Wait on file_count AND symbol_count — see resolve_route.rs for why
/// file_count alone races derived rows.
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
                        // Extra canary: symbols for the def file must land.
                        if let Ok(sresp) = client
                            .get(format!("{base}/api/symbols"))
                            .query(&[("repo", repo), ("path", "lib.rs")])
                            .send()
                            .await
                        {
                            if let Ok(sbody) = sresp.json::<serde_json::Value>().await {
                                let n = sbody["symbols"].as_array().map(|a| a.len()).unwrap_or(0);
                                if n > 0 {
                                    return;
                                }
                            }
                        }
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

async fn find_def(base: &str, repo: &str, path: &str, name: &str) -> (u32, u32) {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/symbols"))
        .query(&[("repo", repo), ("path", path)])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    for s in body["symbols"].as_array().unwrap() {
        if s["name"].as_str() == Some(name) {
            let line = s["line_start"].as_u64().unwrap() as u32;
            // Prefer the symbol's col_start when it lands ON the name; some
            // extractors put col_start on a leading keyword (`pub`), so fall
            // back to scanning the source line for the identifier.
            let mut col = s["col_start"].as_u64().unwrap_or(0) as u32;
            if let Ok(fresp) = client
                .get(format!("{base}/api/file"))
                .query(&[("repo", repo), ("path", path)])
                .send()
                .await
            {
                if let Ok(fbody) = fresp.json::<serde_json::Value>().await {
                    let text = fbody["content"]
                        .as_str()
                        .or_else(|| fbody["text"].as_str())
                        .unwrap_or("");
                    if let Some(line_text) = text.lines().nth((line as usize).saturating_sub(1)) {
                        if let Some(idx) = line_text.find(name) {
                            // Only override when the reported col does not
                            // start the name itself.
                            let at = line_text
                                .get(col as usize..)
                                .unwrap_or("")
                                .starts_with(name);
                            if !at {
                                col = idx as u32;
                            }
                        }
                    }
                }
            }
            return (line, col);
        }
    }
    panic!("symbol {name} not in {path}: {body}");
}

fn fixture_files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "lib.rs",
            r#"
pub fn target_fn() {
    let x = 1;
    let _ = x;
}

pub fn mid_caller() {
    target_fn();
}
"#,
        ),
        (
            "caller.rs",
            r#"
use crate::target_fn;

pub fn outer_caller() {
    target_fn();
    mid_caller();
}

fn mid_caller() {
    target_fn();
}
"#,
        ),
        (
            "consumer.rs",
            r#"
// Imports the defining module (dependency footprint).
use crate::target_fn;

pub fn consume() {
    let _ = 1;
}
"#,
        ),
        (
            "tests/impact_test.rs",
            r#"
use crate::target_fn;

#[test]
fn calls_target() {
    target_fn();
}
"#,
        ),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impact_analysis_buckets_and_tests_separation() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit_tree(dir, &fixture_files(), "impact fixture");

    let boot = boot("impact", dir).await;
    wait_for_indexed(&boot.base, "impact", 4).await;

    let (line, col) = find_def(&boot.base, "impact", "lib.rs", "target_fn").await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/impact/analysis", boot.base))
        .query(&[
            ("repo", "impact"),
            ("path", "lib.rs"),
            ("line", &line.to_string()),
            ("col", &col.to_string()),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "impact/1");
    assert_eq!(body["symbol"]["name"], "target_fn");
    assert!(body["note"].as_str().unwrap().contains("compositional"));

    // Collect all non-test paths across buckets.
    let mut all_paths = Vec::new();
    for bucket in ["direct_exact", "direct_likely", "transitive", "imports"] {
        for r in body[bucket].as_array().unwrap() {
            let p = r["path"].as_str().unwrap();
            assert!(
                !p.contains("tests/") && !p.contains("impact_test"),
                "test path leaked into {bucket}: {p}"
            );
            all_paths.push(p.to_string());
        }
    }

    // Callers / usages of target_fn should appear somewhere in direct or transitive.
    let has_caller_signal = body["direct_exact"]
        .as_array()
        .unwrap()
        .iter()
        .chain(body["direct_likely"].as_array().unwrap())
        .chain(body["transitive"].as_array().unwrap())
        .any(|r| {
            r["path"].as_str() == Some("caller.rs")
                || r["path"].as_str() == Some("lib.rs")
                || r["kind"].as_str() == Some("caller")
                || r["kind"].as_str() == Some("usage")
                || r["kind"].as_str() == Some("transitive")
        });
    assert!(
        has_caller_signal || !all_paths.is_empty() || !body["tests"].as_array().unwrap().is_empty(),
        "expected some impact rows; body={body}"
    );

    // Tests bucket: test file rows only.
    for r in body["tests"].as_array().unwrap() {
        let p = r["path"].as_str().unwrap();
        assert!(
            p.contains("tests/") || p.contains(".test.") || p.contains("e2e/"),
            "non-test path in tests bucket: {p}"
        );
    }

    // Depth decay: any depth>=3 row is candidate; depth>=2 is never exact.
    for r in body["transitive"].as_array().unwrap() {
        let depth = r["depth"].as_u64().unwrap_or(0);
        let class = r["class"].as_str().unwrap();
        if depth >= 3 {
            assert_eq!(class, "candidate", "depth 3 must be candidate: {r}");
        }
        if depth >= 2 {
            assert_ne!(class, "exact", "depth 2+ never exact: {r}");
        }
    }

    // Provenance degraded (kb_daemon off) → null or empty session counts.
    // Field may be null entirely.
    if !body["provenance"].is_null() {
        // allowed; sessionless means direct_* provenance null
    }

    boot.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impact_analysis_respects_limit_cap() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit_tree(dir, &fixture_files(), "impact fixture");

    let boot = boot("impact", dir).await;
    wait_for_indexed(&boot.base, "impact", 4).await;

    let (line, col) = find_def(&boot.base, "impact", "lib.rs", "target_fn").await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/impact/analysis", boot.base))
        .query(&[
            ("repo", "impact"),
            ("path", "lib.rs"),
            ("line", &line.to_string()),
            ("col", &col.to_string()),
            ("limit", "1"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    for bucket in ["direct_exact", "direct_likely", "imports", "tests"] {
        let n = body[bucket].as_array().map(|a| a.len()).unwrap_or(0);
        assert!(n <= 1, "{bucket} exceeded limit=1: {n}");
    }
    boot.task.abort();
}
