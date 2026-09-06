//! V3.1-H2 — `GET /api/lenses` HTTP tests.
//!
//! Counts vs known fixture; author may be human (sessionless fixture is the
//! degraded path — session null, pain null always).

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, SemanticSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::time::{Duration, Instant};

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
            url: "http://127.0.0.1:0".to_string(),
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

/// Poll until the lane this file's assertions actually READ is ready.
///
/// V72-H2b: the gate used to stop at `symbol_count > 0` plus two symbols on
/// `lens.rs`, and then the test asserted on USAGE counts — which come from
/// the `occurrences` table, derived by a pass `ingest::index_file` runs
/// AFTER `replace_symbols`. Symbols landing proves nothing about
/// occurrences, so the gate was one derivation short of what it gated and
/// the test could observe `exact=0 likely=0 cand=0` on a store that was
/// still mid-ingest. Anything that lengthens the ingest path widens that
/// window; the V72-H2b highlight gate did, which is how it surfaced.
///
/// Sibling of V72-H2a's own fix ("review-map readiness gate waited on one
/// symbol, not all four"), and the same rule: a readiness gate waits for
/// the LAST derivation the test reads, never the first one it can see.
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
                        if let Ok(sresp) = client
                            .get(format!("{base}/api/symbols"))
                            .query(&[("repo", repo), ("path", "lens.rs")])
                            .send()
                            .await
                        {
                            if let Ok(sbody) = sresp.json::<serde_json::Value>().await {
                                let n = sbody["symbols"].as_array().map(|a| a.len()).unwrap_or(0);
                                if n >= 2 && alpha_has_usages(&client, base, repo).await {
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
            "index timeout (symbols + alpha usage counts)"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `true` once `GET /api/lenses` reports at least one usage of `alpha` —
/// the occurrences-backed lane the assertions below read.
async fn alpha_has_usages(client: &reqwest::Client, base: &str, repo: &str) -> bool {
    let Ok(resp) = client
        .get(format!("{base}/api/lenses"))
        .query(&[("repo", repo), ("path", "lens.rs")])
        .send()
        .await
    else {
        return false;
    };
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        return false;
    };
    let Some(alpha) = body["declarations"]
        .as_array()
        .and_then(|d| d.iter().find(|d| d["name"] == "alpha"))
    else {
        return false;
    };
    ["exact", "likely", "candidate"]
        .iter()
        .filter_map(|k| alpha["usages"][k].as_u64())
        .sum::<u64>()
        >= 1
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lenses_counts_and_sessionless_author() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit_tree(
        dir,
        &[(
            "lens.rs",
            r#"
/// Doc for alpha.
pub fn alpha() {
    let y = 1;
    let _ = y;
}

pub fn beta() {
    alpha();
}

pub struct Widget;
"#,
        )],
        "lenses fixture",
    );

    let boot = boot("lenses", dir).await;
    wait_for_indexed(&boot.base, "lenses", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/lenses", boot.base))
        .query(&[("repo", "lenses"), ("path", "lens.rs")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "lenses/1");
    assert_eq!(body["path"], "lens.rs");
    assert!(body["total"].as_u64().unwrap() >= 2);

    let decls = body["declarations"].as_array().unwrap();
    let alpha = decls
        .iter()
        .find(|d| d["name"] == "alpha")
        .expect("alpha declaration");
    // alpha is used by beta — at least one likely or candidate usage count
    // (exact may also include same-file locals).
    let exact = alpha["usages"]["exact"].as_u64().unwrap();
    let likely = alpha["usages"]["likely"].as_u64().unwrap();
    let cand = alpha["usages"]["candidate"].as_u64().unwrap();
    assert!(
        exact + likely + cand >= 1,
        "alpha should have usage counts; got exact={exact} likely={likely} cand={cand}"
    );

    let widget = decls.iter().find(|d| d["name"] == "Widget");
    if let Some(w) = widget {
        // types get implementors field (may be 0)
        assert!(w["implementors"].is_number() || w["implementors"].is_null());
    }

    // Sessionless fixture: session null; author may be human (blame) or null
    // if blame failed; pain always null.
    for d in decls {
        assert!(d["pain"].is_null(), "pain must stay null: {d}");
        assert!(
            d["session"].is_null() || d["session"]["id"].is_string(),
            "session shape: {d}"
        );
        // Degraded path under test: no kb sessions → session is null.
        assert!(
            d["session"].is_null(),
            "sessionless fixture must have null session: {d}"
        );
    }

    boot.task.abort();
}
