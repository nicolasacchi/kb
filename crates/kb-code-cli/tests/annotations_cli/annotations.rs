//! `kb-code annotations`/`kb-code annotate` — W4.6 CLI smoke tests against
//! a REAL running kb-code daemon. Mirrors `tests/blame.rs`'s own
//! `boot()`/fixture-repo conventions.
//!
//! D3 extends this file: full CLI parity for annotations v2 — kind flags
//! (`--to`/`--symbol`/`--sha`) on the create form, the thread/lifecycle
//! subcommands (`reply`/`resolve`/`reopen`/`edit`/`set-intent`/`delete`),
//! and the repo-wide `annotations open` listing. Coverage here is the
//! "core" the phase brief calls for (create each kind, reply, resolve,
//! open-listing filter, delete cascade) PLUS reopen/edit/set-intent folded
//! into one combined lifecycle test (`annotate_reply_resolve_reopen_edit_
//! and_set_intent_lifecycle_over_one_thread`) rather than a daemon boot
//! per verb — each boot here is a real daemon + live-mirror walk, so
//! chaining cheap PATCH-only steps over ONE already-booted thread is a
//! meaningful cost saving with no coverage loss (every verb still gets its
//! own assertion). Kind-flag mutual exclusivity + the intent client-side
//! vocab gate are pure `clap`/fn unit tests in `src/main.rs` instead (no
//! daemon needed there at all).

use crate::common::{boot, git};
use assert_cmd::Command;
use predicates::str::contains;
use std::path::Path;
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_then_annotations_round_trips_over_the_cli() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "lib.rs:2",
            "-m",
            "why is fn b here?",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("created"))
        .stdout(contains("L2"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("L2"))
        .stdout(contains("why is fn b here?"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    let anns = body["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0]["line"], 2);
    assert_eq!(anns[0]["stale"], false);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotations_reports_no_annotations_when_none_exist() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("no annotations"));
    task.abort();
}

#[test]
fn annotate_unreachable_daemon_fails_cleanly() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "lib.rs:1",
            "-m",
            "hi",
            "--repo",
            "fixture",
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}

// =====================================================================
// D3 — kind flags (range/symbol/diff), thread/lifecycle verbs, `open`.
// =====================================================================

fn head_sha(dir: &Path) -> String {
    String::from_utf8(
        StdCommand::new("git")
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

/// Poll `GET /api/repos`'s `symbol_count` until it reaches `expected` —
/// mirrors `kb-code-server`'s own `tests/annotations_route.rs::
/// wait_for_symbols` (a private test helper in that crate, so this is a
/// same-shaped local copy rather than a cross-crate reuse): a `--symbol`
/// creation needs the daemon's initial boot walk to have derived the
/// file's symbols first.
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_range_kind_creates_and_lists_with_a_span() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "lib.rs:1",
            "-m",
            "range comment",
            "--to",
            "2",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("created"))
        .stdout(contains("L1\u{2013}2"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("L1\u{2013}2"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_symbol_kind_derives_the_enclosing_symbol_and_the_404_is_friendly() {
    let repo_tmp = symbol_fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;
    // 3 symbols: a, dist, c.
    wait_for_symbols(&url, "fixture", 3).await;

    // Line 4 ("    0.0") sits inside `dist`'s body (lines 3..5) — resolves
    // to the enclosing symbol's line_start (3), not the create-time line.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "lib.rs:4",
            "-m",
            "why does dist do this?",
            "--symbol",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("created"))
        .stdout(contains("L3"));

    // Line 2 is a blank line between `a` and `dist` — no symbol encloses
    // it; the daemon's 404 must render as a friendly nudge, not raw JSON.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "lib.rs:2",
            "-m",
            "no symbol here",
            "--symbol",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .failure()
        .stderr(contains("no enclosing symbol"))
        .stderr(contains("annotate the line instead"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_diff_kind_pins_to_a_commit() {
    let repo_tmp = fixture_repo();
    let sha = head_sha(repo_tmp.path());
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    let short = &sha[..8];
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "lib.rs:2",
            "-m",
            "pinned at c1",
            "--sha",
            &sha,
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("created"))
        .stdout(contains(short.to_string()));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_reply_resolve_reopen_edit_and_set_intent_lifecycle_over_one_thread() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    let create_out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate", "lib.rs:2", "-m", "why b?", "--daemon", &url, "--repo", "fixture", "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let created: serde_json::Value = serde_json::from_slice(&create_out).unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // reply — requires --repo/--path (unlike every verb below).
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "reply",
            &id,
            "-m",
            "it's a stub",
            "--repo",
            "fixture",
            "--path",
            "lib.rs",
            "--intent",
            "question",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("replied"))
        .stdout(contains("[question]"));

    // Threaded listing shows the reply indented under its parent.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("\u{21b3}"));

    // set-intent
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["annotate", "set-intent", &id, "todo", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("[todo]"));

    // resolve
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["annotate", "resolve", &id, "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("resolved"));

    // Default listing now hides the resolved thread entirely.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("no open annotations"));

    // --all shows it again, marked resolved.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--all",
        ])
        .assert()
        .success()
        .stdout(contains("(resolved)"));

    // reopen
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["annotate", "reopen", &id, "--daemon", &url])
        .assert()
        .success();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success()
        .stdout(contains("why b?"));

    // edit
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "edit",
            &id,
            "-m",
            "edited body",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("edited body"));

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotations_open_listing_filters_by_intent_and_path_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp2, url, task) = boot(dir, "fixture").await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "a.rs:1",
            "-m",
            "todo on a",
            "--intent",
            "todo",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success();
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate",
            "b.rs:1",
            "-m",
            "question on b",
            "--intent",
            "question",
            "--daemon",
            &url,
            "--repo",
            "fixture",
        ])
        .assert()
        .success();

    // No filters: both.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["annotations", "open", "--repo", "fixture", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("a.rs"))
        .stdout(contains("b.rs"));

    // intent filter.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "open",
            "--repo",
            "fixture",
            "--intent",
            "todo",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .stdout(contains("a.rs"))
        .stdout(contains("todo on a"));

    // path-prefix filter, --json for the exact shape (reply_count/truncated
    // must be present and correct — this is the D4 hook's future query
    // surface, so the shape matters more here than the human table does).
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "open",
            "--repo",
            "fixture",
            "--path-prefix",
            "b.",
            "--daemon",
            &url,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let anns = body["annotations"].as_array().unwrap();
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0]["path"], "b.rs");
    assert_eq!(anns[0]["reply_count"], 0);
    assert_eq!(body["truncated"], false);

    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn annotate_delete_prompts_unless_yes_and_cascades_to_replies() {
    let repo_tmp = fixture_repo();
    let (_tmp, url, task) = boot(repo_tmp.path(), "fixture").await;

    let create_out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate", "lib.rs:2", "-m", "parent", "--daemon", &url, "--repo", "fixture", "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let created: serde_json::Value = serde_json::from_slice(&create_out).unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotate", "reply", &id, "-m", "child", "--repo", "fixture", "--path", "lib.rs",
            "--daemon", &url,
        ])
        .assert()
        .success();

    // No --yes: assert_cmd's default stdin is closed (immediate EOF), so
    // the confirm prompt reads an empty line -> defaults to "no" -> the
    // annotation must survive.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["annotate", "delete", &id, "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("aborted"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--all",
        ])
        .assert()
        .success()
        .stdout(contains("parent"));

    // --yes deletes for real, cascading to the reply.
    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["annotate", "delete", &id, "--yes", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("deleted"));

    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "annotations",
            "lib.rs",
            "--daemon",
            &url,
            "--repo",
            "fixture",
            "--all",
        ])
        .assert()
        .success()
        .stdout(contains("no annotations"));

    task.abort();
}
