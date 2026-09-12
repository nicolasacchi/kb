//! W4.6 — end-to-end HTTP tests for `GET`/`POST /api/annotations` +
//! `PATCH`/`DELETE /api/annotations/{id}`, against a real daemon booted via
//! `serve_on_random_port_with_paths` over a real git fixture repo. Mirrors
//! `tests/join_route.rs`'s own conventions (`boot_with_repo`-style helper,
//! `SERIAL` guard).
//!
//! Phase D-server adds anchor kinds (`range`/`symbol`/`diff`), threads
//! (`parent_id` replies), intents, and the repo-wide `GET
//! /api/annotations/open` listing — see the tests below `annotation_events_
//! fire_on_the_sse_bus` for that half.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running (the
//! workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

mod common;

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("lib.rs"),
        "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    boot_with_repos(&[(name, path)]).await
}

async fn boot_with_repos(entries: &[(&str, &Path)]) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: entries
            .iter()
            .map(|(name, path)| RepoEntry {
                name: name.to_string(),
                path: std::fs::canonicalize(path).unwrap(),
            })
            .collect(),
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

/// Poll `GET /api/repos`'s `symbol_count` for `repo` until it reaches
/// `expected` — mirrors `tests/agentview_routes.rs`'s own `wait_for_symbols`
/// (needed before a `symbol`-kind creation: the initial boot walk must have
/// derived this file's symbols before `state.store.symbols_for_blob` can
/// find the enclosing one).
async fn wait_for_symbols(base: &str, repo: &str, expected: usize) {
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
                    if count >= expected {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected repo {repo:?} to report symbol_count >= {expected} within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Poll `GET /api/annotations?repo=&path=` until `id`'s view satisfies
/// `pred`, or panic at the deadline. Used for anything whose resolution
/// depends on the live mirror re-indexing an edit (`symbol`'s re-derived
/// enclosing span in particular) — polling the ANNOTATION'S OWN view
/// (rather than an aggregate store count) sidesteps any ambiguity about
/// which internal store state a given poll tick observed: it just waits
/// for the actual client-visible outcome under test.
async fn wait_for_annotation_view(
    base: &str,
    repo: &str,
    path: &str,
    id: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
    what: &str,
) -> serde_json::Value {
    let client = reqwest::Client::new();
    // 45s, was 15s: the mirror-driven re-derive this waits on gets starved
    // under a full parallel `just ci-code` run, and V3.1-H1's salt bumps
    // made the concurrent re-extraction load heavier still (flaked the
    // 2026-08-02 gate; 3/3 green in ~2s isolated). Correctness is asserted
    // by the predicate, not the wait length.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let resp = client
            .get(format!("{base}/api/annotations"))
            .query(&[("repo", repo), ("path", path)])
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = resp.json().await.unwrap();
        if let Some(view) = body["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
        {
            if pred(view) {
                return view.clone();
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn head_sha(dir: &Path) -> String {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotation_crud_round_trip_over_http() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    // Empty list before any annotation exists.
    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["annotations"].as_array().unwrap().is_empty());

    // Create.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 2,
            "body": "why is b() here?",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(created["path"], "lib.rs");
    assert_eq!(created["repo"], "r");
    assert_eq!(created["body"], "why is b() here?");
    assert_eq!(created["author"], "you");
    assert_eq!(created["resolved"], false);
    assert_eq!(created["line"], 2);
    assert_eq!(created["stale"], false);
    assert_eq!(created["anchor"]["kind"], "selection");
    assert_eq!(created["anchor"]["snippet"], "fn b() {}");
    let id = created["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("ann_"));

    // List now returns it, resolved fresh.
    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    let anns = listed["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0]["id"], id);
    assert_eq!(anns[0]["line"], 2);
    assert_eq!(anns[0]["stale"], false);

    // Patch: body + resolved.
    let resp = client
        .patch(format!("{base}/api/annotations/{id}"))
        .json(&serde_json::json!({ "body": "resolved: it's a stub", "resolved": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let patched: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(patched["body"], "resolved: it's a stub");
    assert_eq!(patched["resolved"], true);

    // Delete.
    let resp = client
        .delete(format!("{base}/api/annotations/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    assert!(listed["annotations"].as_array().unwrap().is_empty());

    // Deleting again 404s.
    let resp = client
        .delete(format!("{base}/api/annotations/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotation_anchor_survives_an_upstream_edit_that_shifts_lines() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 2,
            "body": "about fn b",
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["line"], 2);

    // Insert 3 lines above the anchored line (was line 2, now line 5).
    std::fs::write(
        dir.join("lib.rs"),
        "// header 1\n// header 2\n// header 3\nfn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
    )
    .unwrap();

    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    let anns = listed["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0]["id"], id);
    assert_eq!(
        anns[0]["line"], 5,
        "the anchor must follow the shifted line"
    );
    assert_eq!(anns[0]["stale"], false);

    // Now delete the anchored content entirely, leaving nothing textually
    // similar to "fn b() {}" behind -> stale. Deliberately NOT another
    // `fn X() {}`-shaped line: kb-core's Jaro-Winkler resolver is tuned for
    // prose, and two boilerplate lines differing by one character score
    // ABOVE the fuzzy threshold (see `annotations.rs`'s own module doc).
    std::fs::write(
        dir.join("lib.rs"),
        "// header 1\n// header 2\n// header 3\n1\n2\n3\n",
    )
    .unwrap();
    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    let anns = listed["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0]["stale"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_annotation_rejects_an_out_of_range_line() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 999,
            "body": "out of range",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 0,
            "body": "zero is not 1-based",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotation_events_fire_on_the_sse_bus() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    // Connect the SSE stream FIRST so the create below is observed.
    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(sse_resp.status(), reqwest::StatusCode::OK);
    let mut stream = sse_resp.bytes_stream();

    let create = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 1,
            "body": "hello",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), reqwest::StatusCode::CREATED);

    use futures::StreamExt;
    let found = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut buf = String::new();
        loop {
            let chunk = stream.next().await.expect("stream item").unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk));
            if buf.contains("annotation.changed") {
                return true;
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(found, "expected an annotation.changed SSE frame");
}

// =====================================================================
// Phase D-server — anchor kinds (range/symbol/diff), threads, intents,
// and the repo-wide open-annotations listing.
// =====================================================================

fn range_fixture_repo() -> tempfile::TempDir {
    // Deliberately distinctive, non-boilerplate lines (NATO-alphabet-ish
    // words) rather than `fn a() {}`-shaped siblings: kb-core's
    // Jaro-Winkler resolver is tuned for prose, and near-identical
    // boilerplate lines score ABOVE the fuzzy threshold against each other
    // (see `annotations.rs`'s own module doc) — that would let the END
    // anchor falsely fuzzy-match a SURVIVING sibling line instead of
    // genuinely going stale. Distinct vocabulary per line avoids that.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("lib.rs"),
        "header line zero\nalpha bravo charlie delta echo\nfoxtrot golf hotel india juliet\nkilo lima mike november oscar\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_annotation_resolves_both_endpoints_drifts_and_flags_stale() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = range_fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    // Anchor the range on "alpha bravo..." (2, start) .. "foxtrot golf..."
    // (3, end).
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "anchor_kind": "range",
            "line": 2,
            "line_end": 3,
            "body": "range comment",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(created["anchor_kind"], "range");
    assert_eq!(created["line"], 2);
    assert_eq!(created["line_end"], 3);
    assert_eq!(created["stale"], false);
    let id = created["id"].as_str().unwrap().to_string();

    // Shift both endpoints down by 2 lines — each side resolves
    // independently, and both must follow.
    std::fs::write(
        dir.join("lib.rs"),
        "// h1\n// h2\nheader line zero\nalpha bravo charlie delta echo\nfoxtrot golf hotel india juliet\nkilo lima mike november oscar\n",
    )
    .unwrap();
    let view = wait_for_annotation_view(
        &base,
        "r",
        "lib.rs",
        &id,
        |v| v["line"] == 4,
        "the range's start to shift to line 4",
    )
    .await;
    assert_eq!(view["line_end"], 5);
    assert_eq!(view["stale"], false);

    // Now replace ONLY the END side's line with a genuinely unrelated
    // decoy (no remaining line — header/start/trailing — is textually
    // close to it, unlike the replaced original). The END side falls back
    // to its ORIGINAL recorded offset (3), which is now LESS than the
    // START side's freshly-resolved line (4) — proving both the "stale
    // when either side is" rule AND the "normalize/swap" rule in one edit.
    std::fs::write(
        dir.join("lib.rs"),
        "// h1\n// h2\nheader line zero\nalpha bravo charlie delta echo\nthe quick brown fox jumps over the lazy dog\nkilo lima mike november oscar\n",
    )
    .unwrap();
    let view = wait_for_annotation_view(
        &base,
        "r",
        "lib.rs",
        &id,
        |v| v["stale"] == true,
        "the range to go stale once its end anchor's content is gone",
    )
    .await;
    assert_eq!(
        view["line"], 3,
        "swap: the stale end's ORIGINAL offset (3) is now the smaller of the two"
    );
    assert_eq!(view["line_end"], 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_annotation_create_requires_line_end() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "anchor_kind": "range",
            "line": 2, "body": "missing line_end",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

fn symbol_fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("lib.rs"),
        "fn a() {}\n\nfn dist() -> f64 {\n    0.0\n}\n\nfn c() {}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbol_annotation_derives_the_enclosing_symbol_and_follows_it_through_drift() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = symbol_fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    // 3 symbols: a, dist, c.
    wait_for_symbols(&base, "r", 3).await;
    let client = reqwest::Client::new();

    // Line 4 ("    0.0") sits inside `dist`'s body (lines 3..5).
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "anchor_kind": "symbol",
            "line": 4, "body": "why does dist do this?",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(created["anchor_kind"], "symbol");
    // The descriptor's display label travels on the wire (D3's CLI review
    // found clients couldn't say WHICH symbol an annotation follows).
    assert_eq!(created["symbol"], "dist");
    assert_eq!(
        created["line"], 3,
        "resolves to the enclosing symbol's line_start, not the create-time line"
    );
    assert_eq!(created["stale"], false);
    let id = created["id"].as_str().unwrap().to_string();

    // Insert 2 lines above the WHOLE file — `dist` shifts from
    // line_start=3 to line_start=5. A symbol annotation follows the
    // function through drift; a line annotation wouldn't.
    std::fs::write(
        dir.join("lib.rs"),
        "// header1\n// header2\nfn a() {}\n\nfn dist() -> f64 {\n    0.0\n}\n\nfn c() {}\n",
    )
    .unwrap();
    let view = wait_for_annotation_view(
        &base,
        "r",
        "lib.rs",
        &id,
        |v| v["line"] == 5,
        "the symbol annotation to follow dist's shifted line_start (5)",
    )
    .await;
    assert_eq!(view["stale"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbol_annotation_create_404s_when_no_symbol_encloses_the_line() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = symbol_fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    wait_for_symbols(&base, "r", 3).await;
    let client = reqwest::Client::new();

    // Line 2 is a BLANK line between `a` (a one-liner, line_start ==
    // line_end == 1) and `dist` (starts at line 3) — no symbol encloses
    // it.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "anchor_kind": "symbol",
            "line": 2, "body": "no symbol here",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_annotation_pins_an_immutable_position_and_never_re_resolves() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let sha1 = head_sha(dir);
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "anchor_kind": "diff",
            "line": 2, "sha": sha1, "body": "pinned at c1",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(created["anchor_kind"], "diff");
    assert_eq!(created["line"], 2);
    assert_eq!(created["stale"], false);
    assert_eq!(created["sha"], sha1);
    let id = created["id"].as_str().unwrap().to_string();

    // Completely rewrite AND re-commit the working tree — a diff
    // annotation must be entirely unaffected (it never re-resolves).
    std::fs::write(
        dir.join("lib.rs"),
        "totally different content\nmore lines\nand more\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "rewrite"]);

    // No wait_for_* needed: `diff` never consults the working tree at all,
    // so there is nothing to settle — but give the live mirror a tick
    // anyway so a genuine regression (accidentally re-resolving) can't hide
    // behind a race.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    let view = listed["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id)
        .unwrap();
    assert_eq!(view["line"], 2, "a diff annotation must never re-resolve");
    assert_eq!(view["stale"], false);
    assert_eq!(view["sha"], sha1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_annotation_create_rejects_malformed_and_unresolvable_shas() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    // Missing `sha` entirely.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "anchor_kind": "diff",
            "line": 1, "body": "no sha",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Malformed shape (too short / non-hex).
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "anchor_kind": "diff",
            "line": 1, "sha": "zz", "body": "bad shape",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Well-formed but unresolvable (no such commit in this repo).
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "anchor_kind": "diff",
            "line": 1, "sha": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "body": "unresolvable",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_inherits_repo_path_mirrors_parent_line_and_cascades_on_delete() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "why b?",
        }))
        .send()
        .await
        .unwrap();
    let parent: serde_json::Value = resp.json().await.unwrap();
    let parent_id = parent["id"].as_str().unwrap().to_string();
    assert_eq!(parent["line"], 2);

    // A reply: no line/anchor_kind at all, just parent_id.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "it's a stub",
            "parent_id": parent_id, "intent": "question",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let reply: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(reply["parent_id"], parent_id);
    assert!(reply["anchor"].is_null(), "a reply carries no anchor");
    assert_eq!(
        reply["line"], 2,
        "a reply's line mirrors its parent's resolved line"
    );
    assert_eq!(reply["stale"], false);
    assert_eq!(reply["intent"], "question");
    let reply_id = reply["id"].as_str().unwrap().to_string();

    // Reply-to-reply is rejected — one level of nesting only.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "nested reply",
            "parent_id": reply_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Unknown parent 404s.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "orphan reply",
            "parent_id": "ann_does_not_exist",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);

    // Both rows are visible in the plain per-path list.
    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(listed["annotations"].as_array().unwrap().len(), 2);

    // Deleting the PARENT cascades to the reply.
    let resp = client
        .delete(format!("{base}/api/annotations/{parent_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);

    let resp = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap();
    let listed: serde_json::Value = resp.json().await.unwrap();
    assert!(
        listed["annotations"].as_array().unwrap().is_empty(),
        "deleting the parent must cascade-delete its reply"
    );

    // The reply is really gone now, not just orphaned.
    let resp = client
        .delete(format!("{base}/api/annotations/{reply_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_and_patch_reject_invalid_vocab() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    // Invalid anchor_kind.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 1, "body": "x",
            "anchor_kind": "paragraph",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Invalid intent on create.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 1, "body": "x",
            "intent": "urgent",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // A valid annotation to PATCH against.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 1, "body": "x",
        }))
        .send()
        .await
        .unwrap();
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_str().unwrap();

    // Invalid intent on patch.
    let resp = client
        .patch(format!("{base}/api/annotations/{id}"))
        .json(&serde_json::json!({ "intent": "urgent" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // A valid intent patch works and round-trips.
    let resp = client
        .patch(format!("{base}/api/annotations/{id}"))
        .json(&serde_json::json!({ "intent": "todo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let patched: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(patched["intent"], "todo");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn open_annotations_listing_filters_by_intent_and_path_prefix_and_excludes_resolved() {
    let _guard = SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);

    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();

    async fn create(
        client: &reqwest::Client,
        base: &str,
        path: &str,
        intent: &str,
        body: &str,
    ) -> String {
        let resp = client
            .post(format!("{base}/api/annotations"))
            .json(&serde_json::json!({
                "repo": "r", "path": path, "line": 1, "body": body, "intent": intent,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
        let created: serde_json::Value = resp.json().await.unwrap();
        created["id"].as_str().unwrap().to_string()
    }

    let todo_a = create(&client, &base, "a.rs", "todo", "todo on a").await;
    let _question_b = create(&client, &base, "b.rs", "question", "question on b").await;
    let resolved_a = create(&client, &base, "a.rs", "todo", "resolved todo on a").await;
    client
        .patch(format!("{base}/api/annotations/{resolved_a}"))
        .json(&serde_json::json!({ "resolved": true }))
        .send()
        .await
        .unwrap();
    // A reply must never appear in the open listing itself.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "a.rs", "body": "a reply", "parent_id": todo_a,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    // No filters: exactly the two open, top-level annotations.
    let resp = client
        .get(format!("{base}/api/annotations/open"))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let anns = body["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 2);
    assert_eq!(body["truncated"], false);
    let todo_entry = anns.iter().find(|a| a["id"] == todo_a).unwrap();
    assert_eq!(todo_entry["reply_count"], 1);

    // intent filter.
    let resp = client
        .get(format!("{base}/api/annotations/open"))
        .query(&[("repo", "r"), ("intent", "todo")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let anns = body["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0]["id"], todo_a);

    // path_prefix filter.
    let resp = client
        .get(format!("{base}/api/annotations/open"))
        .query(&[("repo", "r"), ("path_prefix", "b.")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let anns = body["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0]["path"], "b.rs");

    // Invalid intent 400s.
    let resp = client
        .get(format!("{base}/api/annotations/open"))
        .query(&[("repo", "r"), ("intent", "bogus")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

// =====================================================================
// V4.C1 — review-scoped create validation
// =====================================================================

async fn create_review_on(client: &reqwest::Client, base: &str, repo: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": "main",
            "base_ref": "main",
            "title": "review-comments create validation",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_scoped_create_validation_matrix() {
    let _guard = SERIAL.lock().await;
    let repo_a = fixture_repo();
    let repo_b = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repos(&[("r", repo_a.path()), ("s", repo_b.path())]).await;
    let client = reqwest::Client::new();

    let review_r = create_review_on(&client, &base, "r").await;
    let review_s = create_review_on(&client, &base, "s").await;

    // Unknown review → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "x",
            "review_id": 999_999,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(
        resp.text().await.unwrap().contains("no such review"),
        "unknown review should name the missing id"
    );

    // Wrong-repo review → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "x",
            "review_id": review_s,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(
        resp.text().await.unwrap().contains("belongs to repo"),
        "wrong-repo review should say so"
    );

    // Unknown ps → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "x",
            "review_id": review_r, "ps": 99,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(
        resp.text().await.unwrap().contains("no patchset 99"),
        "unknown ps should name the missing number"
    );

    // symbol + review_id → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "x",
            "review_id": review_r, "anchor_kind": "symbol",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(resp.text().await.unwrap().contains("line|range"));

    // diff + review_id → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "x",
            "review_id": review_r, "anchor_kind": "diff", "sha": "deadbeef",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(resp.text().await.unwrap().contains("line|range"));

    // bad side → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "x",
            "review_id": review_r, "side": "left",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(resp.text().await.unwrap().contains("invalid side"));

    // Happy path: review-scoped line create defaults side=new, ps=latest.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "about b",
            "review_id": review_r,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    let created: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(created["review_id"], review_r);
    assert_eq!(created["ps_number"], 1);
    assert_eq!(created["side"], "new");
    assert_eq!(created["line"], 2);
    let parent_id = created["id"].as_str().unwrap().to_string();

    // Reply inherits scope when the body omits it.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "ack",
            "parent_id": parent_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let reply: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(reply["review_id"], review_r);
    assert_eq!(reply["ps_number"], 1);
    assert_eq!(reply["side"], "new");
    assert_eq!(reply["parent_id"], parent_id);

    // Reply with a conflicting review_id → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "nope",
            "parent_id": parent_id, "review_id": review_s,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Reply with a conflicting side → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "nope",
            "parent_id": parent_id, "side": "old",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Reply with a conflicting ps → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "nope",
            "parent_id": parent_id, "ps": 99,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    // Plain parent + reply that tries to set review_id → 400
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 3, "body": "plain",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let plain_id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "nope",
            "parent_id": plain_id, "review_id": review_r,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

// --- V4.C2 suggestion storage + batch ------------------------------------

async fn collect_sse_matching<S, B, E>(
    stream: &mut S,
    needle: &str,
    timeout: std::time::Duration,
) -> usize
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    use futures::StreamExt;
    let mut buf = String::new();
    let mut hits = 0usize;
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(chunk.as_ref()));
                while let Some(idx) = buf.find(needle) {
                    hits += 1;
                    buf = buf[idx + needle.len()..].to_string();
                }
            }
            _ => break,
        }
    }
    hits
}

/// V4.C2 — `GET /api/events` replays the bus ring buffer from cursor 0
/// for a fresh connection (`events_stream(&bus, 0)`), so a stream opened
/// mid-test first delivers every event setup already emitted. Consume
/// that backlog (bounded read, result discarded) so the assertions that
/// follow count only NEW events.
async fn drain_sse_backlog<S, B, E>(stream: &mut S)
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    let _ = collect_sse_matching(stream, "event:", std::time::Duration::from_millis(300)).await;
}

fn review_fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("lib.rs"),
        "fn a() {}\nfn b_changed() {}\nfn c() {}\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature"]);
    tmp
}

async fn create_feature_review_on(client: &reqwest::Client, base: &str) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "r",
            "head_ref": "feature",
            "base_ref": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggestion_put_captures_original_from_pinned_blob_and_working_tree() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = review_fixture_repo();
    let dir = repo_tmp.path();
    let (_daemon_tmp, base) = boot_with_repo("r", dir).await;
    let client = reqwest::Client::new();
    let review_id = create_feature_review_on(&client, &base).await;

    // Review-scoped comment on the pinned tip (line 2 = "fn b_changed() {}").
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 2,
            "body": "rename",
            "review_id": review_id,
            "side": "new",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let rev_ann = resp.json::<serde_json::Value>().await.unwrap();
    let rev_id = rev_ann["id"].as_str().unwrap().to_string();

    // Working tree now differs from the pinned blob.
    std::fs::write(
        dir.join("lib.rs"),
        "fn a() {}\nfn WORKING() {}\nfn c() {}\n",
    )
    .unwrap();

    let resp = client
        .put(format!("{base}/api/annotations/{rev_id}/suggestion"))
        .json(&serde_json::json!({ "replacement": "fn b_fixed() {}" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let sug: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(sug["original"], "fn b_changed() {}");
    assert_eq!(sug["replacement"], "fn b_fixed() {}");
    assert_eq!(sug["applied"], false);
    assert!(!sug["base_blob_sha"].as_str().unwrap().is_empty());

    // Plain annotation reads the (now-changed) working tree.
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 2,
            "body": "plain",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let plain_id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = client
        .put(format!("{base}/api/annotations/{plain_id}/suggestion"))
        .json(&serde_json::json!({ "replacement": "fn WT() {}" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let sug: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(sug["original"], "fn WORKING() {}");
    assert_eq!(sug["base_blob_sha"], "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggestion_put_rejects_reply_symbol_and_old_side() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = review_fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();
    let review_id = create_feature_review_on(&client, &base).await;

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 1, "body": "parent",
        }))
        .send()
        .await
        .unwrap();
    let parent_id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "reply",
            "parent_id": parent_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let reply_id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = client
        .put(format!("{base}/api/annotations/{reply_id}/suggestion"))
        .json(&serde_json::json!({ "replacement": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 2, "body": "old side",
            "review_id": review_id, "side": "old",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let old_id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = client
        .put(format!("{base}/api/annotations/{old_id}/suggestion"))
        .json(&serde_json::json!({ "replacement": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    wait_for_symbols(&base, "r", 1).await;
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 1,
            "anchor_kind": "symbol", "body": "sym",
        }))
        .send()
        .await
        .unwrap();
    if resp.status() == reqwest::StatusCode::CREATED {
        let sid = resp.json::<serde_json::Value>().await.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let resp = client
            .put(format!("{base}/api/annotations/{sid}/suggestion"))
            .json(&serde_json::json!({ "replacement": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn suggestion_reput_resets_applied_and_delete_404_emits_sse() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 1, "body": "edit me",
        }))
        .send()
        .await
        .unwrap();
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .put(format!("{base}/api/annotations/{id}/suggestion"))
        .json(&serde_json::json!({ "replacement": "fn a_new() {}" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    assert!(store
        .mark_annotation_suggestion_applied(&id, 99, "deadbeef")
        .unwrap());
    drop(store);

    let resp = client
        .put(format!("{base}/api/annotations/{id}/suggestion"))
        .json(&serde_json::json!({ "replacement": "fn a_newer() {}" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let sug: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(sug["applied"], false);
    assert!(sug["applied_at"].is_null());
    assert_eq!(sug["replacement"], "fn a_newer() {}");

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let resp = client
        .delete(format!("{base}/api/annotations/{id}/suggestion"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    let hits = collect_sse_matching(
        &mut stream,
        "annotation.changed",
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(hits >= 1, "DELETE suggestion must emit annotation.changed");

    let resp = client
        .delete(format!("{base}/api/annotations/{id}/suggestion"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_n_ops_one_sse_created_ids_order_and_cap() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 3, "body": "existing",
        }))
        .send()
        .await
        .unwrap();
    let existing = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let resp = client
        .post(format!("{base}/api/annotations/batch"))
        .json(&serde_json::json!({
            "repo": "r",
            "ops": [
                {"op": "add_comment", "path": "lib.rs", "line": 1, "body": "first"},
                {"op": "add_comment", "path": "lib.rs", "line": 2, "body": "second"},
                {"op": "resolve", "id": existing},
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["applied"], 3);
    assert_eq!(body["changed"], true);
    let created = body["created_ids"].as_array().unwrap();
    assert_eq!(created.len(), 2);

    let listed: serde_json::Value = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let anns = listed["annotations"].as_array().unwrap();
    let first = anns.iter().find(|a| a["id"] == created[0]).unwrap();
    let second = anns.iter().find(|a| a["id"] == created[1]).unwrap();
    assert_eq!(first["body"], "first");
    assert_eq!(second["body"], "second");

    let hits = collect_sse_matching(
        &mut stream,
        "annotation.changed",
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(hits, 1, "N ops must emit exactly one annotation.changed");

    let too_many: Vec<serde_json::Value> = (0..101)
        .map(|i| {
            serde_json::json!({
                "op": "add_comment",
                "path": "lib.rs",
                "line": 1,
                "body": format!("n{i}"),
            })
        })
        .collect();
    let resp = client
        .post(format!("{base}/api/annotations/batch"))
        .json(&serde_json::json!({ "repo": "r", "ops": too_many }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_atomicity_and_all_noop() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let listed_before: serde_json::Value = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let before = listed_before["annotations"].as_array().unwrap().len();

    let resp = client
        .post(format!("{base}/api/annotations/batch"))
        .json(&serde_json::json!({
            "repo": "r",
            "ops": [
                {"op": "add_comment", "path": "lib.rs", "line": 1, "body": "should roll back"},
                {"op": "resolve", "id": "ann_does_not_exist"},
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let listed_after: serde_json::Value = client
        .get(format!("{base}/api/annotations"))
        .query(&[("repo", "r"), ("path", "lib.rs")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        listed_after["annotations"].as_array().unwrap().len(),
        before,
        "failing op must roll back the whole batch"
    );

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 4, "body": "to resolve",
        }))
        .send()
        .await
        .unwrap();
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = client
        .patch(format!("{base}/api/annotations/{id}"))
        .json(&serde_json::json!({ "resolved": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let resp = client
        .post(format!("{base}/api/annotations/batch"))
        .json(&serde_json::json!({
            "repo": "r",
            "ops": [{ "op": "resolve", "id": id }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["changed"], false);
    assert_eq!(body["applied"], 1);

    let hits = collect_sse_matching(
        &mut stream,
        "annotation.changed",
        std::time::Duration::from_millis(400),
    )
    .await;
    assert_eq!(hits, 0, "all-no-op batch must emit no SSE");
}

// --- V4.S1 suggestion apply ----------------------------------------------

async fn create_line_annotation(
    client: &reqwest::Client,
    base: &str,
    line: u32,
    body: &str,
) -> String {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": line, "body": body,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn put_suggestion(client: &reqwest::Client, base: &str, id: &str, replacement: &str) {
    let resp = client
        .put(format!("{base}/api/annotations/{id}/suggestion"))
        .json(&serde_json::json!({ "replacement": replacement }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_happy_path_writes_exact_bytes_marks_applied_emits_sse() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let id = create_line_annotation(&client, &base, 2, "rename b").await;
    put_suggestion(&client, &base, &id, "fn b_new() {}").await;

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let resp = client
        .post(format!("{base}/api/annotations/{id}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["applied"], true);
    assert_eq!(body["changed"], true);
    assert_eq!(body["path"], "lib.rs");
    assert_eq!(body["line"], 2);
    assert!(body.get("line_end").is_none() || body["line_end"].is_null());

    let expected = "fn a() {}\nfn b_new() {}\nfn c() {}\nfn d() {}\n";
    assert_eq!(
        std::fs::read_to_string(dir.join("lib.rs")).unwrap(),
        expected
    );

    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    let sug = store.get_annotation_suggestion(&id).unwrap().unwrap();
    assert!(sug.applied);
    assert!(sug.applied_at.is_some());
    assert_eq!(
        sug.applied_head_sha.as_deref(),
        Some(head_sha(&dir).as_str())
    );

    let hits = collect_sse_matching(
        &mut stream,
        "suggestion.applied",
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(hits >= 1, "apply must emit suggestion.applied");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_drifted_file_is_409_and_bytes_unchanged() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let id = create_line_annotation(&client, &base, 2, "rename b").await;
    put_suggestion(&client, &base, &id, "fn b_new() {}").await;

    std::fs::write(
        dir.join("lib.rs"),
        "fn a() {}\nfn DRIFTED() {}\nfn c() {}\nfn d() {}\n",
    )
    .unwrap();

    let resp = client
        .post(format!("{base}/api/annotations/{id}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("original") || body.get("expected").is_some());
    assert_eq!(body["expected"], "fn b() {}");
    assert_eq!(body["found"], "fn DRIFTED() {}");
    assert_eq!(body["resolved_line"], 2);

    let after = std::fs::read(dir.join("lib.rs")).unwrap();
    assert_eq!(
        after, b"fn a() {}\nfn DRIFTED() {}\nfn c() {}\nfn d() {}\n",
        "409 must leave the drifted file untouched"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_already_equal_replacement_is_noop() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let id = create_line_annotation(&client, &base, 2, "rename b").await;
    put_suggestion(&client, &base, &id, "fn b_new() {}").await;

    let already = "fn a() {}\nfn b_new() {}\nfn c() {}\nfn d() {}\n";
    std::fs::write(dir.join("lib.rs"), already).unwrap();
    // Pin mtime in the past so any rewrite would bump it.
    let status = Command::new("touch")
        .args(["-d", "2000-01-01T00:00:00Z"])
        .arg(dir.join("lib.rs"))
        .status()
        .unwrap();
    assert!(status.success());
    let mtime_before = std::fs::metadata(dir.join("lib.rs"))
        .unwrap()
        .modified()
        .unwrap();

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let resp = client
        .post(format!("{base}/api/annotations/{id}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["already_applied"], true);
    assert_eq!(body["changed"], false);
    assert_eq!(body["applied"], false);

    assert_eq!(
        std::fs::read_to_string(dir.join("lib.rs")).unwrap(),
        already
    );
    let mtime_after = std::fs::metadata(dir.join("lib.rs"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(mtime_before, mtime_after, "already-equal must not write");

    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    let sug = store.get_annotation_suggestion(&id).unwrap().unwrap();
    assert!(!sug.applied, "already-equal must not mark applied");

    let hits = collect_sse_matching(
        &mut stream,
        "suggestion.applied",
        std::time::Duration::from_millis(400),
    )
    .await;
    assert_eq!(hits, 0, "already-equal must emit no suggestion.applied");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_deleted_file_is_409() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let id = create_line_annotation(&client, &base, 2, "rename b").await;
    put_suggestion(&client, &base, &id, "fn b_new() {}").await;
    std::fs::remove_file(dir.join("lib.rs")).unwrap();

    let resp = client
        .post(format!("{base}/api/annotations/{id}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("not found in the working tree"),
        "deleted file must 409 with a clear body, got {body}"
    );
    assert!(!dir.join("lib.rs").exists(), "must not recreate the file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_reply_is_400_and_missing_suggestion_is_404() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let parent = create_line_annotation(&client, &base, 1, "parent").await;
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "body": "reply",
            "parent_id": parent,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let reply_id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = client
        .post(format!("{base}/api/annotations/{reply_id}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);

    let lonely = create_line_annotation(&client, &base, 3, "no suggestion").await;
    let resp = client
        .post(format!("{base}/api/annotations/{lonely}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_multiline_range_with_different_line_count() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 2,
            "line_end": 3,
            "anchor_kind": "range",
            "body": "collapse b+c",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    put_suggestion(&client, &base, &id, "fn bc() {}").await;

    let resp = client
        .post(format!("{base}/api/annotations/{id}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["line"], 2);
    assert_eq!(body["line_end"], 3);
    assert_eq!(
        std::fs::read_to_string(dir.join("lib.rs")).unwrap(),
        "fn a() {}\nfn bc() {}\nfn d() {}\n"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_preserves_trailing_newline_presence_and_absence() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    // File ends with \n (the fixture default).
    let with_nl = create_line_annotation(&client, &base, 1, "a with nl").await;
    put_suggestion(&client, &base, &with_nl, "fn A() {}").await;
    let resp = client
        .post(format!("{base}/api/annotations/{with_nl}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let bytes = std::fs::read(dir.join("lib.rs")).unwrap();
    assert!(bytes.ends_with(b"\n"), "must keep the trailing newline");
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "fn A() {}\nfn b() {}\nfn c() {}\nfn d() {}\n"
    );

    // Rewrite without a trailing newline, then apply a different line.
    std::fs::write(
        dir.join("lib.rs"),
        "fn A() {}\nfn b() {}\nfn c() {}\nfn d() {}",
    )
    .unwrap();
    let no_nl = create_line_annotation(&client, &base, 4, "d without nl").await;
    put_suggestion(&client, &base, &no_nl, "fn D() {}").await;
    let resp = client
        .post(format!("{base}/api/annotations/{no_nl}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let bytes = std::fs::read(dir.join("lib.rs")).unwrap();
    assert!(
        !bytes.ends_with(b"\n"),
        "must not invent a trailing newline"
    );
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        "fn A() {}\nfn b() {}\nfn c() {}\nfn D() {}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_resolve_true_also_resolves_the_annotation() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let id = create_line_annotation(&client, &base, 2, "rename b").await;
    put_suggestion(&client, &base, &id, "fn b_new() {}").await;

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let resp = client
        .post(format!("{base}/api/annotations/{id}/apply"))
        .json(&serde_json::json!({ "resolve": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );

    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    let row = store.get_annotation(&id).unwrap().unwrap();
    assert!(row.resolved, "resolve:true must stamp the annotation");
    let sug = store.get_annotation_suggestion(&id).unwrap().unwrap();
    assert!(sug.applied);

    // One slurp so both events (emitted back-to-back) are counted from
    // the same buffer — a sequential collect would eat the second.
    let frames = slurp_sse(&mut stream, std::time::Duration::from_secs(2)).await;
    assert!(
        frames.contains("suggestion.applied"),
        "apply must emit suggestion.applied; got {frames:?}"
    );
    assert!(
        frames.contains("annotation.changed"),
        "resolve:true must emit annotation.changed once; got {frames:?}"
    );
}

// --- PRR-R10 apply-batch --------------------------------------------------

/// Same as `fixture_repo` but with a SECOND tracked file (`util.rs`), for
/// the multi-file batch tests.
fn fixture_repo_two_files() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("lib.rs"),
        "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
    )
    .unwrap();
    std::fs::write(dir.join("util.rs"), "fn x() {}\nfn y() {}\nfn z() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

async fn create_line_annotation_on(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    line: u32,
    body: &str,
) -> String {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": path, "line": line, "body": body,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create_range_annotation_on(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    line: u32,
    line_end: u32,
    body: &str,
) -> String {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": path, "line": line, "line_end": line_end,
            "anchor_kind": "range", "body": body,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_batch_multi_file_happy_path() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo_two_files();
    let dir = repo_tmp.path().to_path_buf();
    let (daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let id_a = create_line_annotation_on(&client, &base, "lib.rs", 2, "rename b").await;
    put_suggestion(&client, &base, &id_a, "fn b_new() {}").await;
    let id_b = create_line_annotation_on(&client, &base, "lib.rs", 4, "rename d").await;
    put_suggestion(&client, &base, &id_b, "fn d_new() {}").await;
    let id_c = create_line_annotation_on(&client, &base, "util.rs", 1, "rename x").await;
    put_suggestion(&client, &base, &id_c, "fn x_new() {}").await;

    let sse_resp = client
        .get(format!("{base}/api/events"))
        .send()
        .await
        .unwrap();
    let mut stream = sse_resp.bytes_stream();
    drain_sse_backlog(&mut stream).await;

    let resp = client
        .post(format!("{base}/api/annotations/apply-batch"))
        .json(&serde_json::json!({ "annotation_ids": [id_a, id_b, id_c] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["restored"].as_array().unwrap().is_empty());
    assert!(body["failed"].is_null());
    let applied = body["applied"].as_array().unwrap();
    assert_eq!(applied.len(), 3, "{body}");

    assert_eq!(
        std::fs::read_to_string(dir.join("lib.rs")).unwrap(),
        "fn a() {}\nfn b_new() {}\nfn c() {}\nfn d_new() {}\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("util.rs")).unwrap(),
        "fn x_new() {}\nfn y() {}\nfn z() {}\n"
    );

    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    for id in [&id_a, &id_b, &id_c] {
        let sug = store.get_annotation_suggestion(id).unwrap().unwrap();
        assert!(sug.applied, "{id} must be marked applied");
    }

    let hits = collect_sse_matching(
        &mut stream,
        "suggestion.applied",
        std::time::Duration::from_secs(5),
    )
    .await;
    assert_eq!(hits, 3, "one suggestion.applied per item");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_batch_verify_phase_409_with_verdicts_and_files_untouched() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo_two_files();
    let dir = repo_tmp.path().to_path_buf();
    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let good = create_line_annotation_on(&client, &base, "lib.rs", 2, "rename b").await;
    put_suggestion(&client, &base, &good, "fn b_new() {}").await;

    let lib_before = std::fs::read(dir.join("lib.rs")).unwrap();
    let util_before = std::fs::read(dir.join("util.rs")).unwrap();

    let resp = client
        .post(format!("{base}/api/annotations/apply-batch"))
        .json(&serde_json::json!({ "annotation_ids": [good, "ann_does_not_exist"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    let verdicts = body["verdicts"].as_array().unwrap();
    assert_eq!(verdicts.len(), 2, "{body}");
    let good_v = verdicts.iter().find(|v| v["id"] == good).unwrap();
    assert_eq!(good_v["ok"], true, "{body}");
    let bad_v = verdicts
        .iter()
        .find(|v| v["id"] == "ann_does_not_exist")
        .unwrap();
    assert_eq!(bad_v["ok"], false, "{body}");
    assert_eq!(bad_v["error"]["kind"], "not_found", "{body}");

    // Nothing written — byte-compare both files against their pre-call bytes.
    assert_eq!(std::fs::read(dir.join("lib.rs")).unwrap(), lib_before);
    assert_eq!(std::fs::read(dir.join("util.rs")).unwrap(), util_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_batch_same_file_descending_order_correctness() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    // Collapsing lines 1-2 into ONE line shifts every later line up by
    // one — if the line-4 suggestion were applied against that ALREADY
    // shifted content (ascending order) it would land on the wrong text
    // (or past EOF). Descending order (line 4 first, then the 1-2 range)
    // must land both correctly regardless of request order.
    let range_id = create_range_annotation_on(&client, &base, "lib.rs", 1, 2, "collapse a+b").await;
    put_suggestion(&client, &base, &range_id, "fn ab() {}").await;
    let line_id = create_line_annotation_on(&client, &base, "lib.rs", 4, "rename d").await;
    put_suggestion(&client, &base, &line_id, "fn D() {}").await;

    // Request order deliberately ASCENDING (range first) — the handler
    // must still splice in descending order internally.
    let resp = client
        .post(format!("{base}/api/annotations/apply-batch"))
        .json(&serde_json::json!({ "annotation_ids": [range_id, line_id] }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );

    assert_eq!(
        std::fs::read_to_string(dir.join("lib.rs")).unwrap(),
        "fn ab() {}\nfn c() {}\nfn D() {}\n",
        "descending-order splice must preserve both edits"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_batch_overlap_reject() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let range_id = create_range_annotation_on(&client, &base, "lib.rs", 2, 3, "collapse b+c").await;
    put_suggestion(&client, &base, &range_id, "fn bc() {}").await;
    let line_id = create_line_annotation_on(&client, &base, "lib.rs", 3, "rename c").await;
    put_suggestion(&client, &base, &line_id, "fn C() {}").await;

    let before = std::fs::read(dir.join("lib.rs")).unwrap();

    let resp = client
        .post(format!("{base}/api/annotations/apply-batch"))
        .json(&serde_json::json!({ "annotation_ids": [range_id, line_id] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    let verdicts = body["verdicts"].as_array().unwrap();
    assert_eq!(verdicts.len(), 2);
    for v in verdicts {
        assert_eq!(v["ok"], false, "{body}");
        assert_eq!(v["error"]["kind"], "overlap", "{body}");
    }
    assert_eq!(std::fs::read(dir.join("lib.rs")).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_batch_already_applied_item_is_verdict_error_not_silent_skip() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let dir = repo_tmp.path().to_path_buf();
    let (_daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let applied_id = create_line_annotation_on(&client, &base, "lib.rs", 2, "rename b").await;
    put_suggestion(&client, &base, &applied_id, "fn b_new() {}").await;
    let resp = client
        .post(format!("{base}/api/annotations/{applied_id}/apply"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let fresh_id = create_line_annotation_on(&client, &base, "lib.rs", 4, "rename d").await;
    put_suggestion(&client, &base, &fresh_id, "fn d_new() {}").await;

    let before = std::fs::read(dir.join("lib.rs")).unwrap();

    let resp = client
        .post(format!("{base}/api/annotations/apply-batch"))
        .json(&serde_json::json!({ "annotation_ids": [applied_id, fresh_id] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.unwrap();
    let verdicts = body["verdicts"].as_array().unwrap();
    let applied_v = verdicts.iter().find(|v| v["id"] == applied_id).unwrap();
    assert_eq!(applied_v["ok"], false, "{body}");
    assert_eq!(applied_v["error"]["kind"], "already_applied", "{body}");
    let fresh_v = verdicts.iter().find(|v| v["id"] == fresh_id).unwrap();
    assert_eq!(
        fresh_v["ok"], true,
        "an individually-fine id must NOT be silently dropped from the verdict list: {body}"
    );

    // The whole batch aborts: fresh_id's suggestion must NOT have landed.
    assert_eq!(std::fs::read(dir.join("lib.rs")).unwrap(), before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_batch_resolve_threads_resolves_every_applied_annotation() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo_two_files();
    let dir = repo_tmp.path().to_path_buf();
    let (daemon_tmp, base) = boot_with_repo("r", &dir).await;
    let client = reqwest::Client::new();

    let id_a = create_line_annotation_on(&client, &base, "lib.rs", 1, "rename a").await;
    put_suggestion(&client, &base, &id_a, "fn A() {}").await;
    let id_b = create_line_annotation_on(&client, &base, "util.rs", 1, "rename x").await;
    put_suggestion(&client, &base, &id_b, "fn X() {}").await;

    let resp = client
        .post(format!("{base}/api/annotations/apply-batch"))
        .json(&serde_json::json!({
            "annotation_ids": [id_a, id_b],
            "resolve_threads": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );

    let db = daemon_tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    for id in [&id_a, &id_b] {
        let row = store.get_annotation(id).unwrap().unwrap();
        assert!(row.resolved, "{id} must be resolved by resolve_threads");
        let sug = store.get_annotation_suggestion(id).unwrap().unwrap();
        assert!(sug.applied);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_batch_cap_400() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let ids: Vec<String> = (0..51).map(|i| format!("ann_fake_{i}")).collect();
    let resp = client
        .post(format!("{base}/api/annotations/apply-batch"))
        .json(&serde_json::json!({ "annotation_ids": ids }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
}

async fn slurp_sse<S, B, E>(stream: &mut S, timeout: std::time::Duration) -> String
where
    S: futures::Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    use futures::StreamExt;
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => buf.push_str(&String::from_utf8_lossy(chunk.as_ref())),
            _ => break,
        }
    }
    buf
}
