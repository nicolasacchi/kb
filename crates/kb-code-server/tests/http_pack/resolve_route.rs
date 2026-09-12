//! B2 — end-to-end HTTP tests for `GET /api/resolve`, against a real daemon
//! booted via `serve_on_random_port_with_paths`. Mirrors `tests/
//! agentview_routes.rs`'s own conventions (`boot`-style helper,
//! `wait_for_indexed`) — the ranking/classification logic is already
//! pin-tested at the unit level in `crates/kb-code-server/src/resolve.rs`
//! (no daemon boot needed there); this file proves the HTTP wiring itself:
//! query params, JSON shape, and status codes.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, SemanticSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::time::{Duration, Instant};

fn commit(dir: &Path, file: &str, contents: &str, message: &str) {
    let path = dir.join(file);
    // B4's import-heuristic tests are the first in this file to commit a
    // NESTED path (`src/lib.rs`) — every prior fixture used a flat
    // top-level filename, so this never needed a parent-dir creation
    // before now.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
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

async fn boot(repo_name: &str, repo_dir: &Path) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        semantic: SemanticSection::default(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

async fn task_abort(boot: Boot) {
    boot.task.abort();
}

/// Same `file_count` poll `tests/agentview_routes.rs` uses — see that
/// file's own doc for why this is sufficient proof symbols/occurrences are
/// indexed too (both derive inside the SAME `ingest::index_file` call that
/// upserts the `files` row).
async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    // file_count races the per-file DERIVED rows (occurrences
                    // land after the file row; V3.G2 widened that window with
                    // import/param extraction — same flake class as
                    // todos_route's 2026-07-31 fix). symbol_count comes from
                    // the same extraction visit that writes occurrences, so a
                    // non-zero count means the fixture's derived rows landed.
                    let symbols = entry["symbol_count"].as_u64().unwrap_or(0);
                    if count >= expected_files && symbols > 0 {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected repo {repo:?} to report file_count >= {expected_files} within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll `/api/resolve` until the position's occurrence-derived `role` lands.
/// Occurrences can lag file_count/symbol_count under a loaded parallel gate
/// (2026-08-02: role came back null on the same fixture that passes 5/5
/// isolated) — the wait targets exactly the data class the assertions read.
/// Returns the settled body (or the last body at deadline — the assertions
/// then fail with the real shape).
async fn resolve_when_ready(
    base: &str,
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
) -> serde_json::Value {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let resp = client
            .get(format!("{base}/api/resolve"))
            .query(&[
                ("repo", repo),
                ("path", path),
                ("line", &line.to_string()),
                ("col", &col.to_string()),
            ])
            .send()
            .await
            .unwrap();
        if resp.status() == reqwest::StatusCode::OK {
            let body: serde_json::Value = resp.json().await.unwrap();
            if body["role"].is_string() || Instant::now() >= deadline {
                return body;
            }
        } else {
            assert!(
                Instant::now() < deadline,
                "resolve stayed non-OK ({}) past the deadline",
                resp.status()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_route_file_local_candidate_ranks_first() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "a.rs",
        "fn widget() -> i32 {\n    1\n}\n\nfn main() {\n    widget();\n}\n",
        "c1",
    );

    let boot = boot("fixture", dir).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    // Click on the call site "widget();" — line 6, col 4 (0-based, on the
    // 'w'). Settle-probe: see resolve_when_ready's doc.
    let body = resolve_when_ready(&boot.base, "fixture", "a.rs", 6, 4).await;
    assert_eq!(body["schema"], "resolve/1");
    assert_eq!(body["ident"], "widget");
    assert_eq!(body["role"], "ref");
    let candidates = body["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty(), "body: {body}");
    // V3.G1: scope-proven locals arm ranks above non-scope file-local.
    assert_eq!(candidates[0]["precision"], "locals");
    assert_eq!(candidates[0]["class"], "exact");
    assert_eq!(candidates[0]["line"], 1);
    assert_eq!(candidates[0]["kind"], "fn");
    assert!(
        body["note"].as_str().unwrap().contains("exact")
            || body["note"].as_str().unwrap().contains("candidate")
    );

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_route_falls_back_to_word_scan_for_a_non_token_level_language() {
    // YAML (a CST-walk key-path outline, ADR-7) is NOT one of
    // `lang::TOKEN_LEVEL_LANG_IDS` even after B5b widened it to eight
    // languages — see that const's own doc for why YAML/TOML/JSON stay out
    // permanently. Python moved INTO the token-level set in B5b, so it no
    // longer fits this test's premise (see `resolve.rs`'s own
    // `fallback_word_scan_when_the_blob_has_no_occurrences_rows` unit test,
    // which uses the SAME YAML-style "genuinely uncovered language" shape).
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.yaml", "widget: 1\n", "c1");

    let boot = boot("fixture", dir).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/resolve", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.yaml"),
            ("line", "1"),
            ("col", "4"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ident"], "widget");
    assert!(body["role"].is_null(), "body: {body}");

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_route_out_of_range_position_400s() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\n", "c1");

    let boot = boot("fixture", dir).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/resolve", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.rs"),
            ("line", "999"),
            ("col", "0"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    task_abort(boot).await;
}

/// B4 — the `"import-heuristic"` tier at the HTTP level: clicking a
/// call site of an imported name resolves to its DEFINING file (`src/b.rs`)
/// ranked above a same-named decoy symbol elsewhere in the repo
/// (`src/c.rs`), which still shows up but only at the fleet-wide
/// `"tags-approx"` tier — see `resolve.rs`'s own unit test of the same
/// shape (`import_heuristic_ranks_above_same_repo_tags_for_the_same_name`)
/// for the ranking logic itself; this proves the HTTP wiring carries it
/// through end to end.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_route_import_heuristic_ranks_above_tags_approx() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "src/lib.rs",
        "use b::widget;\n\nfn main() {\n    widget();\n}\n",
        "c1",
    );
    // A decoy same-named symbol in a DIFFERENT directory (not same-dir —
    // V3.G2 would promote a `src/` sibling to import-filtered). Written
    // before the next commit so `commit`'s own `git add -A` sweeps it in.
    std::fs::create_dir_all(dir.join("other")).unwrap();
    std::fs::write(
        dir.join("other/c.rs"),
        "pub fn widget() -> i32 {\n    999\n}\n",
    )
    .unwrap();
    commit(
        dir,
        "src/b.rs",
        "pub fn widget() -> i32 {\n    42\n}\n",
        "c2",
    );

    let boot = boot("fixture", dir).await;
    wait_for_indexed(&boot.base, "fixture", 3).await;

    let client = reqwest::Client::new();
    // Click on the call site "widget();" — line 4, col 4.
    let resp = client
        .get(format!("{}/api/resolve", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "src/lib.rs"),
            ("line", "4"),
            ("col", "4"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ident"], "widget");
    let candidates = body["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty(), "body: {body}");
    assert_eq!(candidates[0]["precision"], "import-heuristic");
    assert_eq!(candidates[0]["path"], "src/b.rs");
    assert_eq!(candidates[0]["line"], 1);
    assert!(candidates.iter().any(|c| {
        c["path"] == "other/c.rs" && c["precision"] == "tags-approx" && c["class"] == "candidate"
    }));

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_route_unknown_repo_404s() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\n", "c1");

    let boot = boot("fixture", dir).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/resolve", boot.base))
        .query(&[
            ("repo", "no-such-repo"),
            ("path", "a.rs"),
            ("line", "1"),
            ("col", "0"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    task_abort(boot).await;
}
