//! V3.G2 — end-to-end HTTP tests for `GET /api/usages`.

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

async fn wait_for_files(base: &str, repo: &str, n: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    if entry["file_count"].as_u64().unwrap_or(0) as usize >= n {
                        return;
                    }
                }
            }
        }
        assert!(Instant::now() < deadline, "index timeout");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usages_happy_path_and_access_tags() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);

    // Assignment fixture: x written then read; comment/string have the
    // word but must not appear as usage rows (occurrences skip them).
    let src = r#"
let mut x = 1;
x = 2;
let y = x + 1;
// x is only a comment
let s = "x";
fn use_x() {
    let _ = x;
}
"#;
    commit_tree(dir, &[("main.rs", src)], "usages fixture");

    let boot = boot("usages", dir).await;
    wait_for_files(&boot.base, "usages", 1).await;
    // Give occurrences a moment (file_count alone can race).
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = reqwest::Client::new();
    // Position on the `x` in `let y = x + 1` (line 4 of the raw string —
    // leading newline means line 1 is empty... count carefully).
    // Lines (1-based):
    // 1: empty
    // 2: let mut x = 1;
    // 3: x = 2;
    // 4: let y = x + 1;
    let resp = client
        .get(format!("{}/api/usages", boot.base))
        .query(&[
            ("repo", "usages"),
            ("path", "main.rs"),
            ("line", "4"),
            ("col", "8"), // `x` in `let y = x + 1`
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "usages/1");
    assert_eq!(body["symbol"]["name"], "x");

    // Collect all rows across groups.
    let mut all = Vec::new();
    for g in ["exact", "likely", "candidate"] {
        if let Some(arr) = body[g].as_array() {
            for r in arr {
                all.push((
                    g,
                    r["path"].as_str().unwrap_or("").to_string(),
                    r["line"].as_u64().unwrap_or(0),
                    r["access"].as_str().map(|s| s.to_string()),
                    r["context"].as_str().unwrap_or("").to_string(),
                ));
            }
        }
    }
    assert!(
        !all.is_empty(),
        "expected at least one usage row; body={body}"
    );

    // Write access on assignment `x = 2` (line 3).
    let has_write = all.iter().any(|(_, _, line, access, ctx)| {
        *line == 3 && access.as_deref() == Some("write") && ctx.contains("x = 2")
    });
    assert!(
        has_write,
        "expected write-tagged row for assignment; rows={all:?}"
    );

    // Read access on `let y = x + 1` or use_x body.
    let has_read = all
        .iter()
        .any(|(_, _, _, access, _)| access.as_deref() == Some("read"));
    assert!(has_read, "expected at least one read; rows={all:?}");

    // Comment/string exclusion: no row whose context is only the comment
    // or string line.
    let bad = all
        .iter()
        .any(|(_, _, _, _, ctx)| ctx.contains("only a comment") || ctx.trim() == "let s = \"x\";");
    assert!(!bad, "comment/string leaked into usages; rows={all:?}");

    boot.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usages_truncation_with_limit_1() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);

    // Many refs to the same name.
    let src = r#"
fn foo() {}
fn a() { foo(); }
fn b() { foo(); }
fn c() { foo(); }
fn d() { foo(); }
"#;
    commit_tree(dir, &[("many.rs", src)], "many refs");

    let boot = boot("usages2", dir).await;
    wait_for_files(&boot.base, "usages2", 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = reqwest::Client::new();
    // Position on def `foo` at line 2.
    let resp = client
        .get(format!("{}/api/usages", boot.base))
        .query(&[
            ("repo", "usages2"),
            ("path", "many.rs"),
            ("line", "2"),
            ("col", "3"),
            ("limit", "1"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();

    // With limit=1, at least one group that had >1 raw hit should truncate.
    // total_* counts pre-cap.
    let total_refs = body["total_exact"].as_u64().unwrap_or(0)
        + body["total_likely"].as_u64().unwrap_or(0)
        + body["total_candidate"].as_u64().unwrap_or(0);
    if total_refs > 1 {
        assert_eq!(
            body["truncated"].as_bool(),
            Some(true),
            "expected truncated=true when totals>{total_refs}; body={body}"
        );
    }
    // Each group array length ≤ 1.
    for g in ["exact", "likely", "candidate"] {
        let n = body[g].as_array().map(|a| a.len()).unwrap_or(0);
        assert!(n <= 1, "{g} has {n} rows under limit=1");
    }

    boot.task.abort();
}

// --- V71-E1: `GET /api/usages/2` ------------------------------------------

/// The v2 route, end to end, over a Ruby repo — the one test that proves
/// the route is actually REACHABLE (a handler nothing routes to is the
/// same dead surface as a registry row with no handler) and that the
/// enrichment reaches the wire: closed-vocabulary `kind`, the role bitset,
/// per-row `precision`, `enclosing`, `blob_sha`, in-band totals, and the
/// Ruby STRICT verdict.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usages_v2_ruby_strict_lane_and_enrichment() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);

    // `subtotal` is a plain method local: unambiguous, not a method on the
    // hierarchy, no dynamic construct in the enclosing method → STRICT.
    let src = r#"class Invoice
  def total
    subtotal = 1
    subtotal + 2
  end
end
"#;
    commit_tree(dir, &[("invoice.rb", src)], "ruby usages fixture");

    let boot = boot("usages_v2", dir).await;
    wait_for_files(&boot.base, "usages_v2", 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = reqwest::Client::new();
    // line 4 = `    subtotal + 2`, col 4 = the `subtotal` reference.
    let resp = client
        .get(format!("{}/api/usages/2", boot.base))
        .query(&[
            ("repo", "usages_v2"),
            ("path", "invoice.rb"),
            ("line", "4"),
            ("col", "4"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "usages/2");
    assert_eq!(body["symbol"]["name"], "subtotal");

    // The Ruby lane ran and minted exact.
    assert_eq!(body["ruby_strict"]["exact"], true, "body={body}");
    assert_eq!(body["ruby_strict"]["verdict"], "strict");
    assert_eq!(body["ruby_strict"]["hierarchy"][0], "Invoice");

    let exact = body["exact"].as_array().cloned().unwrap_or_default();
    assert!(!exact.is_empty(), "expected exact rows; body={body}");
    for row in &exact {
        assert_eq!(
            row["precision"], "ruby-locals-strict",
            "every exact row here comes from the STRICT lane; row={row}"
        );
        assert_eq!(row["trust"], "exact");
        assert!(
            row["blob_sha"].as_str().is_some_and(|s| !s.is_empty()),
            "row is pinned to bytes; row={row}"
        );
        // Kythe's childof — the enclosing method, not just the file.
        assert_eq!(row["enclosing"]["name"], "total", "row={row}");
        // A closed-vocabulary kind, and its role names decode the bitset.
        let kind = row["kind"].as_str().unwrap_or("");
        assert!(
            ["def", "read", "write", "mutate", "call", "unclassified"].contains(&kind),
            "unexpected kind {kind}"
        );
        let bits = row["roles"].as_u64().unwrap_or(0);
        let names: Vec<&str> = row["role_names"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(
            names.contains(&"definition"),
            bits & 0x1 != 0,
            "role_names must decode roles; row={row}"
        );
    }
    // Totals are in band and there is no cap to report.
    assert_eq!(body["totals"]["exact"], exact.len() as u64);
    assert_eq!(
        body["capped"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "nothing was hidden, so `capped` must be empty"
    );
    assert!(body["kind_totals"].is_object());

    // v1 keeps serving its own shape at its own path, unchanged.
    let v1: serde_json::Value = client
        .get(format!("{}/api/usages", boot.base))
        .query(&[
            ("repo", "usages_v2"),
            ("path", "invoice.rb"),
            ("line", "4"),
            ("col", "4"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v1["schema"], "usages/1");
    assert!(
        v1["ruby_strict"].is_null() && v1["totals"].is_null(),
        "usages/1 must not gain v2 fields: {v1}"
    );

    boot.task.abort();
}

/// A cap is never silent: `capped` names the group, what was returned and
/// the TRUE total.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usages_v2_reports_its_cap_in_band_with_the_true_total() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);

    let src = r#"
fn foo() {}
fn a() { foo(); }
fn b() { foo(); }
fn c() { foo(); }
fn d() { foo(); }
"#;
    commit_tree(dir, &[("many.rs", src)], "many refs");

    let boot = boot("usages_v2_cap", dir).await;
    wait_for_files(&boot.base, "usages_v2_cap", 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/usages/2", boot.base))
        .query(&[
            ("repo", "usages_v2_cap"),
            ("path", "many.rs"),
            ("line", "2"),
            ("col", "3"),
            ("limit", "1"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let all = body["totals"]["all"].as_u64().unwrap_or(0);
    let returned: usize = ["exact", "likely", "candidate"]
        .iter()
        .map(|g| body[g].as_array().map(|a| a.len()).unwrap_or(0))
        .sum();
    if all as usize > returned {
        let capped = body["capped"].as_array().cloned().unwrap_or_default();
        assert!(
            !capped.is_empty(),
            "rows were hidden with no `capped` entry — a silent cap; body={body}"
        );
        for c in &capped {
            assert!(c["total"].as_u64().unwrap_or(0) > c["returned"].as_u64().unwrap_or(0));
            assert_eq!(c["reason"], "page");
        }
    }
    for g in ["exact", "likely", "candidate"] {
        assert!(body[g].as_array().map(|a| a.len()).unwrap_or(0) <= 1);
    }

    boot.task.abort();
}
