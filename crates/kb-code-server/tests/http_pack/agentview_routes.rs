//! W5.1 + W5.2 — end-to-end HTTP tests for the agent context verbs
//! (`GET /api/{map,pack,defs,xrefs,similar,impact}`), against a real daemon
//! booted via `serve_on_random_port_with_paths`. Mirrors `tests/
//! provenance_routes.rs`'s own conventions (`boot`-style helper,
//! `wait_for_indexed`) — most of the RANKING/PARSING logic is already
//! pin-tested at the unit level inside `crates/kb-code-server/src/
//! agentview/*.rs` (no daemon boot needed there); this file proves the
//! HTTP wiring itself: query params, JSON shape, status codes, and one
//! full `pack/1` golden covering every section.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, SemanticSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn commit(dir: &Path, file: &str, contents: &str, message: &str) -> String {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    String::from_utf8(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
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

async fn boot(repo_name: &str, repo_dir: &Path, semantic: SemanticSection) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        semantic,
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

/// Poll `GET /api/repos`'s `file_count` for `repo` until it reaches
/// `expected_files` — mirrors `tests/e2e_daemon.rs`'s own established
/// convention for this exact wait. Deliberately NOT `GET /api/file`
/// succeeding (`tests/provenance_routes.rs`'s own `wait_for_indexed`, which
/// this file's tests do NOT reuse): that route reads working-tree bytes
/// straight off disk, independent of `state.store`, so it says nothing
/// about whether the background initial-index walk has actually run yet —
/// `map`/`pack`/`defs` all read the STORE's `symbols` table
/// (`store::Store::symbols_for_repo`), which `ingest::index_file`
/// populates in the SAME call that upserts the `files` row — but as a
/// SEPARATE store call after it (`upsert_file`, then `replace_symbols`),
/// so there IS a small window where `file_count` has reached the expected
/// total while the last file's symbols are still being derived (observed
/// once under heavy host load, B2 review 2026-07-21 — the walk got ~40%
/// heavier with the occurrences pass). Single-file fixtures shrug (the
/// window is one parse); multi-file fixtures that assert on the symbols
/// table should use [`wait_for_symbols`] as well.
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
                    if count >= expected_files {
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

/// Companion wait for multi-file fixtures whose asserts read the `symbols`
/// table: `/api/repos`' `symbol_count` only reaches the expected total once
/// every file's `replace_symbols` has landed — closing the
/// files-row-before-symbols window `wait_for_indexed`'s doc describes.
async fn wait_for_symbols(base: &str, repo: &str, expected_symbols: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["symbol_count"].as_u64().unwrap_or(0) as usize;
                    if count >= expected_symbols {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected repo {repo:?} to report symbol_count >= {expected_symbols} within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// --- map ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn map_route_returns_a_ranked_outline() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "a.rs",
        "fn one() {}\nfn two() {}\nfn three() {}\n",
        "c1",
    );
    commit(dir, "b.rs", "fn only() {}\n", "c2");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 2).await;
    // a.rs's 3 fns + b.rs's 1 — the map asserts below read the symbols
    // table, so the files-row wait alone is a race (see wait_for_indexed).
    wait_for_symbols(&boot.base, "fixture", 4).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/map", boot.base))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "map/1");
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "body: {body}");
    // a.rs has 3 symbols vs b.rs's 1 — must rank first under v1-rank.
    assert_eq!(files[0]["path"], "a.rs");
    assert!(body["outline"].as_str().unwrap().starts_with("a.rs\n"));

    // A small budget must truncate.
    let small: serde_json::Value = client
        .get(format!("{}/api/map", boot.base))
        .query(&[("repo", "fixture"), ("budget", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(small["truncated"], true);
    task_abort(boot).await;
}

async fn task_abort(boot: Boot) {
    boot.task.abort();
}

// --- pack ----------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_route_golden_covers_every_section() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "known_file.rs",
        "fn a() {}\nfn b() {}\n",
        "the subject\n\nKb-Session: sess-pack\n",
    );

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;
    // pack asserts on outline/symbols — derived rows race the files row
    // (see wait_for_indexed's doc; flaked under V3.G2's heavier walk).
    wait_for_symbols(&boot.base, "fixture", 2).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "fixture"), ("paths", "known_file.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "pack/1");
    assert_eq!(body["repo"], "fixture");
    assert_eq!(body["paths"], serde_json::json!(["known_file.rs"]));
    assert_eq!(body["truncated"], false);

    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    let f = &files[0];
    assert_eq!(f["path"], "known_file.rs");

    // Section 1: map outline.
    assert!(f["outline"].as_str().unwrap().contains("known_file.rs"));
    assert_eq!(f["symbols"].as_array().unwrap().len(), 2);

    // Section 2: provenance summary — the trailer session must dominate.
    let provenance = f["provenance"].as_array().unwrap();
    assert!(!provenance.is_empty(), "body: {body}");
    assert_eq!(provenance[0]["session_id"], "sess-pack");
    assert_eq!(provenance[0]["confidence"], "trailer");

    // Section 3: annotations — empty here because none were created for
    // this fixture (V71-X1 populates the field; see the
    // `pack_route_populates_open_annotations...` test below for a
    // non-empty case). The two budget fields are ALWAYS present, even on
    // an empty pack.
    assert_eq!(f["annotations"], serde_json::json!([]));
    assert_eq!(body["annotations_tokens_used"], 0);
    assert_eq!(body["annotations_budget"], 600); // 15% of the 4000 default.

    // Section 4: recent story entries.
    let story = f["story"].as_array().unwrap();
    assert!(!story.is_empty());
    assert!(story
        .iter()
        .any(|e| e["session_id"].as_str() == Some("sess-pack")));

    // Section 5: content — small file, must fit whole.
    assert_eq!(f["content"]["status"], "full");
    assert_eq!(f["content"]["text"], "fn a() {}\nfn b() {}\n");

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_route_budget_truncates_the_larger_of_two_files() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "small.rs", "fn s() {}\n", "small");
    commit(dir, "big.rs", &format!("// {}\n", "x".repeat(400)), "big");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 2).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("paths", "big.rs,small.rs"),
            ("budget", "10"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["truncated"], true);
    let files = body["files"].as_array().unwrap();
    // Response order matches the REQUESTED order (big.rs first) even though
    // small.rs was visited first internally (smallest-first fill).
    assert_eq!(files[0]["path"], "big.rs");
    assert_eq!(files[1]["path"], "small.rs");
    assert_eq!(files[1]["content"]["status"], "full");
    assert_ne!(files[0]["content"]["status"], "full");

    task_abort(boot).await;
}

/// V71-X1 — `pack`'s step 3: an open `range` annotation on a requested
/// path is resolved and carries `first_line`/`line_count`; a REVIEW-scoped
/// comment on the SAME path is excluded (the Review Room's own surface,
/// see pack.rs's module doc).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_route_populates_open_annotations_with_first_line_and_line_count() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\nfn b() {}\nfn c() {}\n", "c1");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let created: serde_json::Value = client
        .post(format!("{}/api/annotations", boot.base))
        .json(&serde_json::json!({
            "repo": "fixture",
            "path": "a.rs",
            "line": 1,
            "line_end": 2,
            "anchor_kind": "range",
            "body": "please tighten this up",
            "intent": "flag-for-agent",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created["line"], 1);
    assert_eq!(created["line_end"], 2);

    let body: serde_json::Value = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "fixture"), ("paths", "a.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let f = &body["files"][0];
    let anns = f["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1, "body: {body}");
    assert_eq!(anns[0]["intent"], "flag-for-agent");
    assert_eq!(anns[0]["anchor_kind"], "range");
    assert_eq!(anns[0]["body"], "please tighten this up");
    assert_eq!(anns[0]["first_line"], 1);
    assert_eq!(anns[0]["line_count"], 2, "line_end(2) - first_line(1) + 1");
    assert!(body["annotations_tokens_used"].as_u64().unwrap() > 0);
    assert!(
        body["annotations_tokens_used"].as_u64().unwrap()
            <= body["annotations_budget"].as_u64().unwrap()
    );

    task_abort(boot).await;
}

/// A review-scoped comment (`review_id` set) is excluded from `pack`'s
/// annotations section — that's the Review Room's own surface.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_route_excludes_review_scoped_comments() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\n", "c1");
    git(dir, &["checkout", "-q", "-b", "feature"]);
    commit(dir, "a.rs", "fn a() { /* x */ }\n", "c2");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let review: serde_json::Value = client
        .post(format!("{}/api/reviews", boot.base))
        .json(&serde_json::json!({
            "repo": "fixture",
            "head_ref": "feature",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let review_id = review["id"]
        .as_i64()
        .unwrap_or_else(|| panic!("review id: {review}"));

    let created_review_ann: serde_json::Value = client
        .post(format!("{}/api/annotations", boot.base))
        .json(&serde_json::json!({
            "repo": "fixture",
            "path": "a.rs",
            "line": 1,
            "body": "review-scoped, must not leak into pack",
            "intent": "flag-for-agent",
            "review_id": review_id,
            "side": "new",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        created_review_ann["review_id"], review_id,
        "sanity: the annotation must actually be review-scoped for this test to mean anything: {created_review_ann}"
    );

    let body: serde_json::Value = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "fixture"), ("paths", "a.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["files"][0]["annotations"],
        serde_json::json!([]),
        "body: {body}"
    );

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pack_route_unknown_path_404s() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\n", "c1");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let status = client
        .get(format!("{}/api/pack", boot.base))
        .query(&[("repo", "fixture"), ("paths", "does-not-exist.rs")])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    task_abort(boot).await;
}

// --- defs / refs -----------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn defs_route_exact_match_via_http() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn distinctive_name() {}\n", "c1");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;
    // defs reads the symbols table directly — same derived-row race.
    wait_for_symbols(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/defs", boot.base))
        .query(&[("repo", "fixture"), ("symbol", "distinctive_name")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["schema"], "defs/1");
    assert_eq!(body["exact"], true);
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["approximate"], false);
    assert_eq!(results[0]["name"], "distinctive_name");

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refs_route_word_boundary_via_http_excludes_substring_matches() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn foo() {}\nfn foobar() {}\nfoo();\n", "c1");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/xrefs", boot.base))
        .query(&[("repo", "fixture"), ("symbol", "foo")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["schema"], "refs/1");
    let results = body["results"].as_array().unwrap();
    assert_eq!(results.len(), 2, "body: {body}");
    assert!(results.iter().all(|r| r["approximate"] == true));
    assert!(results
        .iter()
        .all(|r| !r["text"].as_str().unwrap().contains("foobar")));

    task_abort(boot).await;
}

// --- similar ---------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn similar_route_400s_with_a_hint_when_semantic_is_disabled() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\n", "c1");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/similar", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.rs"),
            ("start", "1"),
            ("end", "1"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"].as_str().unwrap().contains("semantic"),
        "body: {body}"
    );

    task_abort(boot).await;
}

/// Env-gated, REAL embedder end-to-end for `similar` — mirrors `tests/
/// semantic_e2e.rs`'s own gate (`KB_CODE_SEMANTIC_E2E=1`); NOT run by
/// `just ci-code` (a real model download + a real ONNX subprocess).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn similar_route_real_embed_excludes_the_source_span_and_finds_the_sibling() {
    if std::env::var("KB_CODE_SEMANTIC_E2E").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping similar_route_real_embed_excludes_the_source_span_and_finds_the_sibling: \
             set KB_CODE_SEMANTIC_E2E=1 to run (downloads jina-embeddings-v2-base-code, ~320 MB, \
             and spawns a real kb-embedder subprocess)"
        );
        return;
    }

    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "math.rs",
        "/// Adds two integers.\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\n\
         /// Multiplies two integers together.\npub fn multiply(a: i32, b: i32) -> i32 {\n    a * b\n}\n",
        "c1",
    );

    let boot = boot(
        "fixture",
        dir,
        SemanticSection {
            enabled: true,
            repos: vec!["fixture".to_string()],
            nice: None,
        },
    )
    .await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    // Give the background semantic indexer a window to chunk+embed the
    // freshly-indexed file before querying.
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(120);
    let hits: Vec<serde_json::Value> = loop {
        let resp = client
            .get(format!("{}/api/similar", boot.base))
            .query(&[
                ("repo", "fixture"),
                ("path", "math.rs"),
                ("start", "2"),
                ("end", "4"),
            ])
            .send()
            .await
            .unwrap();
        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await.unwrap();
            let hits = body["hits"].as_array().cloned().unwrap_or_default();
            if !hits.is_empty() || Instant::now() >= deadline {
                break hits;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    assert!(
        !hits.is_empty(),
        "expected the multiply() chunk to surface as similar to add()'s span"
    );
    // The queried span (add(), lines 2-4) must never appear in its own results.
    assert!(!hits.iter().any(|h| {
        h["path"] == "math.rs"
            && h["span_start"].as_u64().unwrap_or(0) <= 4
            && h["span_end"].as_u64().unwrap_or(u64::MAX) >= 2
    }));

    task_abort(boot).await;
}

// --- impact ------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impact_route_reports_co_change_counts_via_http() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "target.rs", "v1", "c1");
    std::fs::write(dir.join("sibling.rs"), "v1").unwrap();
    git(dir, &["add", "-A"]);
    std::fs::write(dir.join("target.rs"), "v2").unwrap();
    git(dir, &["add", "-A"]);
    git(
        dir,
        &["commit", "-q", "-m", "c2: target + sibling together"],
    );

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 2).await;

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{}/api/impact", boot.base))
        .query(&[("repo", "fixture"), ("path", "target.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "impact/1");
    assert_eq!(body["approximate"], true);
    assert_eq!(body["commits_walked"], 2);
    let results = body["results"].as_array().unwrap();
    assert!(
        results
            .iter()
            .any(|r| r["path"] == "sibling.rs" && r["co_changes"] == 1),
        "body: {body}"
    );
    assert!(!results.iter().any(|r| r["path"] == "target.rs"));

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn impact_route_unknown_repo_404s() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "v1", "c1");

    let boot = boot("fixture", dir, SemanticSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let status = client
        .get(format!("{}/api/impact", boot.base))
        .query(&[("repo", "no-such-repo"), ("path", "a.rs")])
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, reqwest::StatusCode::NOT_FOUND);

    task_abort(boot).await;
}
