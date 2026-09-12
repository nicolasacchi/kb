//! Phase C-server ("The Operable Reader," time-first-class) — end-to-end
//! HTTP tests for `GET /api/commit`, `GET /api/compare`,
//! `GET /api/branches`, and `GET /api/file-history`, against a real daemon
//! booted via `serve_on_random_port_with_paths` over real git fixture
//! repos. Mirrors `tests/join_route.rs`/`tests/refs_diff_routes.rs`'s own
//! conventions (`boot_with_repo`-style helper, the `SERIAL` guard, a real
//! `git` fixture built via `std::process::Command`, `disabled_kb_daemon`
//! from `tests/agentview_routes.rs`) rather than sharing them — this crate
//! has no `tests/support` module yet (see `refs_diff_routes.rs`'s own doc
//! for why each e2e file duplicates this small helper set).
//!
//! **These four routes read GIT, not this daemon's own store/index** — like
//! `refs`/`diff` before them, none of them needs `wait_for_indexed`/
//! `wait_for_symbols`-style polling for the boot-time HEAD-tree walk to
//! finish; the daemon can answer them the moment `serve_on_random_port_with_paths`
//! returns. Each test below notes this inline where it'd otherwise look
//! like an oversight.
//!
//! `[kb_daemon] enabled = false` on every boot (`disabled_kb_daemon`) — the
//! join-ladder attribution these routes surface (`commit`/`branches`) must
//! never risk reaching a real kb daemon that happens to be listening on
//! the default `127.0.0.1:4000` on the machine running the suite; the
//! trailer arm (pure local) still fully exercises attribution with
//! federation off.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running
//! (the workspace CLAUDE.md build tips) — `tempfile::tempdir()` honours it.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: "http://127.0.0.1:0".to_string(),
        token_file: None,
        public_url: None,
    }
}

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"))
}

// --- GET /api/commit -------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_reports_full_metadata_trailers_numstat_and_attribution() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "line1\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "root commit"]);

    std::fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    let msg = "the subject\n\nbody paragraph\n\nKb-Session: sess-commit-page\n";
    let status = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["commit", "-q", "-m", msg])
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000500 +0000")
        .status()
        .unwrap();
    assert!(status.success());
    let sha = git_out(&dir, &["rev-parse", "HEAD"]);

    // Pure git reads — no store/index involved, no wait_for_indexed needed.
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/commit"))
        .query(&[("repo", "fixture"), ("sha", &sha[..8])])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["schema"], "commit/1");
    assert_eq!(
        body["sha"], sha,
        "a short prefix must resolve to the full sha"
    );
    assert_eq!(body["subject"], "the subject");
    assert!(body["body"]
        .as_str()
        .unwrap()
        .contains("Kb-Session: sess-commit-page"));
    assert_eq!(body["author"]["name"], "Test");
    assert_eq!(body["author"]["email"], "test@example.com");
    assert_eq!(body["author"]["time"], 1_700_000_000);
    assert_eq!(body["committer"]["time"], 1_700_000_500);
    assert_eq!(body["parents"].as_array().unwrap().len(), 1);
    let trailers = body["trailers"].as_array().unwrap();
    assert!(trailers
        .iter()
        .any(|t| t["key"] == "Kb-Session" && t["value"] == "sess-commit-page"));

    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "a.txt");
    assert_eq!(files[0]["status"], "M");
    assert_eq!(files[0]["insertions"], 1);
    assert_eq!(body["totals"]["files"], 1);
    assert_eq!(body["totals"]["insertions"], 1);

    // Attribution via the join ladder's trailer arm — pure local, works
    // with `[kb_daemon] enabled = false`.
    assert_eq!(body["attribution"]["schema"], "join/1");
    assert_eq!(body["attribution"]["confidence"], "trailer");
    assert_eq!(body["attribution"]["session_id"], "sess-commit-page");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_on_a_root_commit_reports_zero_parents_and_an_added_file() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "line1\nline2\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "root commit"]);
    let sha = git_out(&dir, &["rev-parse", "HEAD"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/commit"))
        .query(&[("repo", "fixture"), ("sha", sha.as_str())])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["parents"].as_array().unwrap().len(), 0);
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["path"], "a.txt");
    assert_eq!(files[0]["status"], "A");
    assert_eq!(files[0]["insertions"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_rejects_a_malformed_sha_with_400() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/commit"))
        .query(&[("repo", "fixture"), ("sha", "zz")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_reports_404_for_an_unresolvable_sha() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/commit"))
        .query(&[("repo", "fixture"), ("sha", "deadbeefdeadbeefdead")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// --- GET /api/compare --------------------------------------------------

/// `main` gets 1 base commit + 1 main-only commit; `feature` (branched off
/// the base) gets 2 feature-only commits.
fn diverged_fixture(dir: &Path) {
    init_repo(dir);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["branch", "feature"]);
    git(dir, &["checkout", "-q", "feature"]);
    std::fs::write(dir.join("b.txt"), "f1\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature one"]);
    std::fs::write(dir.join("c.txt"), "f2\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature two"]);
    git(dir, &["checkout", "-q", "main"]);
    std::fs::write(dir.join("d.txt"), "m1\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "main one"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compare_two_dot_lists_commits_and_files_unique_to_to() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);

    // Pure git reads — no wait_for_indexed needed.
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/compare"))
        .query(&[("repo", "fixture"), ("from", "main"), ("to", "feature")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "compare/1");
    assert_eq!(body["three_dot"], false);
    assert!(body["resolved"]["merge_base"].is_string());
    let subjects: Vec<&str> = body["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, vec!["feature two", "feature one"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compare_three_dot_ranges_from_the_merge_base_excluding_from_only_changes() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);
    let base_sha = git_out(&dir, &["merge-base", "main", "feature"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/compare"))
        .query(&[
            ("repo", "fixture"),
            ("from", "main"),
            ("to", "feature"),
            ("three_dot", "true"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["resolved"]["merge_base"], base_sha);
    let paths: Vec<&str> = body["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"b.txt"));
    assert!(paths.contains(&"c.txt"));
    assert!(
        !paths.contains(&"d.txt"),
        "three-dot compare must exclude main-only changes: {paths:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compare_identical_refs_returns_empty_lists_not_an_error() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/compare"))
        .query(&[("repo", "fixture"), ("from", "main"), ("to", "main")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["commits"].as_array().unwrap().len(), 0);
    assert_eq!(body["files"].as_array().unwrap().len(), 0);
}

/// Phase G-server — absent (default) `attribution` leaves `commits[]`
/// structurally IDENTICAL to the pre-Phase-G shape: no commit entry gains
/// an `attribution` key at all (`CompareCommitOut`'s `skip_serializing_if`
/// on a `None`), so a client that never asks for it never pays for (or
/// sees) the ladder resolution.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compare_attribution_flag_absent_is_byte_identical_to_plain_commit_summary() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/compare"))
        .query(&[("repo", "fixture"), ("from", "main"), ("to", "feature")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let commits = body["commits"].as_array().unwrap();
    assert!(!commits.is_empty());
    for c in commits {
        let keys: std::collections::BTreeSet<&str> =
            c.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from(["sha", "subject", "author", "author_time"]),
            "a flag-off compare must never carry an `attribution` key: {c:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compare_attribution_flag_true_resolves_each_commit_via_the_join_ladder() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/compare"))
        .query(&[
            ("repo", "fixture"),
            ("from", "main"),
            ("to", "feature"),
            ("attribution", "true"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let commits = body["commits"].as_array().unwrap();
    assert!(!commits.is_empty());
    for c in commits {
        assert_eq!(c["attribution"]["schema"], "join/1");
        // `[kb_daemon] enabled = false` on this boot — no trailer, no
        // federation, so every commit degrades to an honest "none".
        assert_eq!(c["attribution"]["confidence"], "none");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compare_rejects_a_dash_prefixed_from_with_400() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/compare"))
        .query(&[
            ("repo", "fixture"),
            ("from", "--output=/tmp/pwned"),
            ("to", "feature"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --- GET /api/branches ---------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_reports_ahead_behind_default_and_attribution() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);

    // Pure git reads — no wait_for_indexed needed.
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "branches/1");
    assert_eq!(body["default"], "main");
    assert_eq!(body["truncated"], false);
    let branches = body["branches"].as_array().unwrap();

    let main = branches.iter().find(|b| b["name"] == "main").unwrap();
    assert_eq!(main["is_head"], true);
    assert_eq!(main["ahead"], 0);
    assert_eq!(main["behind"], 0);
    assert_eq!(main["last"]["subject"], "main one");

    let feature = branches.iter().find(|b| b["name"] == "feature").unwrap();
    assert_eq!(feature["is_head"], false);
    assert_eq!(feature["ahead"], 2, "feature has 2 commits main lacks");
    assert_eq!(feature["behind"], 1, "feature lacks main's 1 own commit");
    assert_eq!(feature["last"]["subject"], "feature two");
    assert_eq!(feature["attribution"]["schema"], "join/1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_on_an_unknown_repo_404s() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "nope")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// Plant `refs/remotes/origin/<name>` + optional origin/HEAD without a
/// network fetch (V4.L1 fixture recipe).
fn plant_origin_ref(dir: &Path, name: &str, sha: &str) {
    git(
        dir,
        &["update-ref", &format!("refs/remotes/origin/{name}"), sha],
    );
}

fn plant_origin_head(dir: &Path, branch: &str) {
    git(
        dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            &format!("refs/remotes/origin/{branch}"),
        ],
    );
}

fn commit_dated(dir: &Path, file: &str, contents: &str, msg: &str, unix: i64) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", file]);
    let date = format!("{unix} +0000");
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", msg])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_enumerates_remotes_dedups_local_and_skips_origin_head() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);
    let main_sha = git_out(&dir, &["rev-parse", "main"]);
    let feature_sha = git_out(&dir, &["rev-parse", "feature"]);
    // Local-wins: origin/feature shares the short name with a local branch.
    plant_origin_ref(&dir, "main", &main_sha);
    plant_origin_ref(&dir, "feature", &feature_sha);
    plant_origin_ref(&dir, "only-on-origin", &main_sha);
    plant_origin_head(&dir, "main");

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["default"], "main");
    let branches = body["branches"].as_array().unwrap();
    let names: Vec<&str> = branches
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"only-on-origin"), "got: {names:?}");
    assert!(
        !names.contains(&"HEAD"),
        "origin/HEAD must not appear: {names:?}"
    );

    let remote_only = branches
        .iter()
        .find(|b| b["name"] == "only-on-origin")
        .unwrap();
    assert_eq!(remote_only["remote"], "origin");

    // Local-wins: one `feature`, and it has no remote field.
    let features: Vec<_> = branches.iter().filter(|b| b["name"] == "feature").collect();
    assert_eq!(features.len(), 1, "local feature must drop origin/feature");
    assert!(
        features[0].get("remote").is_none() || features[0]["remote"].is_null(),
        "local-wins feature must not carry remote: {}",
        features[0]
    );

    let mains: Vec<_> = branches.iter().filter(|b| b["name"] == "main").collect();
    assert_eq!(mains.len(), 1);
    assert!(mains[0].get("remote").is_none() || mains[0]["remote"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_default_follows_origin_head_then_falls_back() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);
    let main_sha = git_out(&dir, &["rev-parse", "main"]);
    plant_origin_ref(&dir, "main", &main_sha);
    plant_origin_ref(&dir, "develop", &main_sha);
    plant_origin_head(&dir, "develop");

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let with_origin: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        with_origin["default"], "develop",
        "origin/HEAD must win over HEAD=main"
    );

    // A second repo with no origin/HEAD keeps the HEAD heuristic.
    let repo2 = tempfile::tempdir().unwrap();
    let dir2 = std::fs::canonicalize(repo2.path()).unwrap();
    diverged_fixture(&dir2);
    git(&dir2, &["checkout", "-q", "feature"]);
    let (_tmp2, base2) = boot_with_repo("fixture", &dir2).await;
    let fallback: serde_json::Value = client
        .get(format!("{base2}/api/branches"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        fallback["default"], "feature",
        "HEAD heuristic when origin/HEAD is absent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_sort_suggested_is_deterministic_and_recency_monotone() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    // Recency is 2^(-age / 14d) against wall-clock now. Absolute 2020/2023
    // unix times both sit ~1000–2000 days back, so the recency term
    // underflows the f32 score and ahead=1 ties fresh/stale. Date the
    // tips relative to now (same deltas as the unit test) so the term
    // actually moves the score.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    commit_dated(&dir, "a.txt", "base\n", "base", now - 180 * 24 * 3600);

    git(&dir, &["checkout", "-q", "-b", "stale"]);
    commit_dated(&dir, "s.txt", "old\n", "stale tip", now - 90 * 24 * 3600);

    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["checkout", "-q", "-b", "fresh"]);
    commit_dated(&dir, "f.txt", "new\n", "fresh tip", now - 3600);

    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["branch", "zeta"]);
    git(&dir, &["branch", "alpha"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let fetch = || async {
        client
            .get(format!("{base}/api/branches"))
            .query(&[("repo", "fixture"), ("sort", "suggested")])
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };
    let first = fetch().await;
    let second = fetch().await;
    let names = |body: &serde_json::Value| -> Vec<String> {
        body["branches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(names(&first), names(&second), "suggested order is stable");

    let branches = first["branches"].as_array().unwrap();
    for b in branches {
        let suggest = &b["suggest"];
        assert!(
            suggest["score"].is_number(),
            "missing score on {}",
            b["name"]
        );
        assert!(
            suggest["terms"].is_object() && !suggest["terms"].as_object().unwrap().is_empty(),
            "terms must be present on {}",
            b["name"]
        );
        assert!(
            suggest["terms"].get("recency").is_some(),
            "recency term on {}",
            b["name"]
        );
    }

    let fresh = branches.iter().find(|b| b["name"] == "fresh").unwrap();
    let stale = branches.iter().find(|b| b["name"] == "stale").unwrap();
    let fresh_r = fresh["suggest"]["terms"]["recency"].as_f64().unwrap();
    let stale_r = stale["suggest"]["terms"]["recency"].as_f64().unwrap();
    assert!(
        fresh_r > stale_r,
        "fresh recency {fresh_r} must exceed stale {stale_r}"
    );
    let fresh_s = fresh["suggest"]["score"].as_f64().unwrap();
    let stale_s = stale["suggest"]["score"].as_f64().unwrap();
    assert!(
        fresh_s > stale_s,
        "fresh score {fresh_s} must exceed stale {stale_s}"
    );

    let order = names(&first);
    let fresh_i = order.iter().position(|n| n == "fresh").unwrap();
    let stale_i = order.iter().position(|n| n == "stale").unwrap();
    assert!(fresh_i < stale_i, "fresh must rank before stale: {order:?}");

    // Name-sort default: alpha before zeta; suggested with identical
    // tips (same commit as main) still name-tie-breaks.
    let name_sort: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let name_order = names(&name_sort);
    let alpha_i = name_order.iter().position(|n| n == "alpha").unwrap();
    let zeta_i = name_order.iter().position(|n| n == "zeta").unwrap();
    assert!(alpha_i < zeta_i, "name-sort: {name_order:?}");
    for b in name_sort["branches"].as_array().unwrap() {
        assert!(
            b.get("suggest").is_none() || b["suggest"].is_null(),
            "name-sort must not carry suggest: {b}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_suggested_truncates_after_rank_name_sort_stays_lex() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    // Date tips relative to NOW: with a 14-day half-life, absolute
    // 2020/2023 epochs both underflow f32 recency to ~0 against
    // wall-clock now, and the ranking silently degenerates to the
    // ahead term (same fragility the recency-monotone fixture fixed).
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    commit_dated(&dir, "a.txt", "base\n", "base", now - 90 * 24 * 3600);

    // 99 lex-first branches sharing the old tip, plus one lex-last
    // recent branch. Cap is 100 including main → 101 total.
    for i in 0..99 {
        git(&dir, &["branch", &format!("aaa-{i:03}")]);
    }
    git(&dir, &["checkout", "-q", "-b", "zzz-recent"]);
    commit_dated(&dir, "z.txt", "new\n", "recent tip", now - 3600);
    git(&dir, &["checkout", "-q", "main"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let by_name: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture"), ("sort", "name")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(by_name["truncated"], true);
    let name_names: Vec<&str> = by_name["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(name_names.len(), 100);
    assert!(
        !name_names.contains(&"zzz-recent"),
        "name-sort must drop the lex-last branch: {name_names:?}"
    );

    let suggested: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture"), ("sort", "suggested")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(suggested["truncated"], true);
    let sug_names: Vec<&str> = suggested["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(sug_names.len(), 100);
    assert!(
        sug_names.contains(&"zzz-recent"),
        "suggested must keep the recent lex-last branch: {sug_names:?}"
    );
    assert_eq!(sug_names[0], "zzz-recent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_has_open_review_when_head_ref_matches() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "fixture",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "open on feature",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        create.status(),
        201,
        "create review: {}",
        create.text().await.unwrap()
    );

    let body: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let feature = body["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "feature")
        .unwrap();
    let main = body["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "main")
        .unwrap();
    assert_eq!(feature["has_open_review"], true);
    assert_eq!(main["has_open_review"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_has_open_review_matches_a_full_ref_head_ref_too() {
    // V70-A3X — `reviews::create_review` stores `head_ref` VERBATIM,
    // exactly as the caller sent it (`reviews.rs`'s own body, no
    // normalization): today's CLI/SPA always send the bare short name, but
    // nothing stops a caller from sending an already-qualified full ref
    // (`refs/heads/feature`). The OLD short-name-only comparison
    // (`open_heads.contains(&r.name)`) could NEVER match that shape at
    // all — this pins the fix's `head_ref == full_ref` arm.
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let create = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "fixture",
            "head_ref": "refs/heads/feature",
            "base_ref": "main",
            "title": "full-ref head_ref",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), 201, "{}", create.text().await.unwrap());

    let body: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let branches = body["branches"].as_array().unwrap();
    let feature = branches.iter().find(|b| b["name"] == "feature").unwrap();
    let main = branches.iter().find(|b| b["name"] == "main").unwrap();
    assert_eq!(feature["has_open_review"], true, "{feature}");
    assert_eq!(main["has_open_review"], false, "{main}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_ahead_behind_are_null_when_there_is_nothing_to_compare() {
    // V70-A3X: a detached HEAD with no `origin/HEAD` either has no default
    // branch to diff against at all — `ahead`/`behind` must report `null`
    // ("nothing to compare"), never a silent `0` indistinguishable from a
    // genuinely measured zero-diff.
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    diverged_fixture(&dir);
    let feature_sha = git_out(&dir, &["rev-parse", "feature"]);
    git(&dir, &["checkout", "-q", &feature_sha]); // detach HEAD

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(body["default"].is_null(), "body: {body}");
    let branches = body["branches"].as_array().unwrap();
    assert!(!branches.is_empty());
    for b in branches {
        assert!(b["ahead"].is_null(), "{b}");
        assert!(b["behind"].is_null(), "{b}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branches_suggested_two_phase_widening_surfaces_an_ahead_heavy_branch_past_the_cheap_cap() {
    // V70-A3X — the two-phase-widening fix's own regression test: a branch
    // whose ONLY differentiator is its `ahead` count (an EXPENSIVE term,
    // computed only for survivors of the cheap pre-rank) is deliberately
    // positioned OUTSIDE the top MAX_BRANCHES=100 by the cheap rank alone
    // (every candidate here is cheap-tied — identical tip author_time, no
    // open review — so the cheap rank is a pure alphabetical tie-break).
    // Before this fix, that meant it could NEVER surface: the OLD
    // single-phase cap truncated to exactly 100 BEFORE `ahead` was ever
    // measured for anything past position 100.
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    commit_dated(&dir, "a.txt", "base\n", "base", now - 90 * 24 * 3600);

    // 104 lex-ordered "b-*" branches, ALL sharing main's tip — cheap-tied
    // (identical recency, no open review, no attribution/ahead/behind in
    // the cheap pass), so their cheap-rank position is purely alphabetical
    // by construction. "b-102" is the 103rd branch alphabetically (past
    // MAX_BRANCHES=100) — it gets 10 extra commits dated the SAME
    // timestamp as everyone else's tip, so its RECENCY term stays
    // identical to its siblings; only `ahead` (measured, never cheap-rank
    // visible) can distinguish it.
    for i in 0..104 {
        git(&dir, &["branch", &format!("b-{i:03}")]);
    }
    git(&dir, &["checkout", "-q", "b-102"]);
    for j in 0..10 {
        commit_dated(
            &dir,
            &format!("extra{j}.txt"),
            "x\n",
            &format!("extra {j}"),
            now - 90 * 24 * 3600,
        );
    }
    git(&dir, &["checkout", "-q", "main"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/branches"))
        .query(&[("repo", "fixture"), ("sort", "suggested")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["truncated"], true, "body: {body}");
    let names: Vec<&str> = body["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 100, "got {names:?}");
    assert!(
        names.contains(&"b-102"),
        "an ahead-heavy branch ranked outside the CHEAP top-100 must still \
         surface once the expensive terms are measured for the widened \
         3xMAX_BRANCHES candidate set: {names:?}"
    );
    let winner = body["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "b-102")
        .unwrap();
    assert_eq!(winner["ahead"], 10, "{winner}");
    assert_eq!(
        names[0], "b-102",
        "its ahead-driven score must dominate the cheap-tied field: {names:?}"
    );
}

// --- GET /api/file-history -----------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_history_follows_a_rename_newest_first() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "c2"]);
    git(&dir, &["mv", "a.txt", "renamed.txt"]);
    git(&dir, &["commit", "-q", "-m", "rename it"]);

    // Pure git reads — no wait_for_indexed needed.
    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/file-history"))
        .query(&[("repo", "fixture"), ("path", "renamed.txt")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "file-history/1");
    assert_eq!(body["truncated"], false);
    let subjects: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, vec!["rename it", "c2", "c1"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_history_respects_limit_and_reports_truncation() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    for (contents, msg) in [("1\n", "c1"), ("2\n", "c2"), ("3\n", "c3")] {
        std::fs::write(dir.join("a.txt"), contents).unwrap();
        git(&dir, &["add", "a.txt"]);
        git(&dir, &["commit", "-q", "-m", msg]);
    }

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/file-history"))
        .query(&[("repo", "fixture"), ("path", "a.txt"), ("limit", "2")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(body["truncated"], true);
    assert_eq!(entries[0]["subject"], "c3");
    assert_eq!(entries[1]["subject"], "c2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_history_rejects_a_path_traversal_attempt() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"one\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/file-history"))
        .query(&[("repo", "fixture"), ("path", "../etc/passwd")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

// --- GET /api/file/stops, GET /api/file/at (V76-R3d) ----------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_stops_follows_rename_reports_floor_and_true_total() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    commit_dated(&dir, "a.txt", "one\n", "c1", 1_700_000_000);
    commit_dated(&dir, "a.txt", "one\ntwo\n", "c2", 1_700_001_000);
    git(&dir, &["mv", "a.txt", "renamed.txt"]);
    let status = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["commit", "-q", "-m", "rename it"])
        .env("GIT_AUTHOR_DATE", "1700002000 +0000")
        .env("GIT_COMMITTER_DATE", "1700002000 +0000")
        .status()
        .unwrap();
    assert!(status.success());

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base}/api/file/stops"))
        .query(&[("repo", "fixture"), ("path", "renamed.txt")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["schema"], "scrub/1");
    assert_eq!(body["truncated"], false);
    assert_eq!(body["total"], 3);
    let subjects: Vec<&str> = body["stops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, vec!["rename it", "c2", "c1"]);
    assert_eq!(body["stops"][0]["renamed_from"], "a.txt");
    assert_eq!(body["floor"]["when"], 1_700_000_000);

    let limited: serde_json::Value = client
        .get(format!("{base}/api/file/stops"))
        .query(&[("repo", "fixture"), ("path", "renamed.txt"), ("limit", "2")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(limited["stops"].as_array().unwrap().len(), 2);
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["total"], 3);
    assert_eq!(limited["floor"], body["floor"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_at_exact_nearest_prior_and_before_floor() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    commit_dated(&dir, "a.txt", "one\n", "c1", 1_700_000_000);
    commit_dated(&dir, "a.txt", "two\n", "c2", 1_700_001_000);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();

    let exact: serde_json::Value = client
        .get(format!("{base}/api/file/at"))
        .query(&[("repo", "fixture"), ("path", "a.txt"), ("at", "1700001000")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(exact["schema"], "scrub/1");
    assert_eq!(exact["resolution"], "exact");
    assert_eq!(exact["stop"]["subject"], "c2");
    assert_eq!(exact["encoding"], "utf8");
    assert_eq!(exact["content"], "two\n");
    assert_eq!(exact["frame"]["lane"], "file_at_ref");

    let prior: serde_json::Value = client
        .get(format!("{base}/api/file/at"))
        .query(&[("repo", "fixture"), ("path", "a.txt"), ("at", "1700000500")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prior["resolution"], "nearest-prior");
    assert_eq!(prior["stop"]["subject"], "c1");
    assert_eq!(prior["content"], "one\n");

    let miss = client
        .get(format!("{base}/api/file/at"))
        .query(&[("repo", "fixture"), ("path", "a.txt"), ("at", "1699999999")])
        .send()
        .await
        .unwrap();
    assert_eq!(miss.status(), 404);
    let body: serde_json::Value = miss.json().await.unwrap();
    assert_eq!(body["type"], "urn:kb:errors:before-floor");
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("1700000000"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_stops_refuses_an_over_cap_limit() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    commit_dated(&dir, "a.txt", "1\n", "c1", 1_700_000_000);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/api/file/stops"))
        .query(&[("repo", "fixture"), ("path", "a.txt"), ("limit", "9999")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("9999"), "{err}");
    assert!(err.contains("500"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_scrub_reads_side_branch_and_rejects_option_refs() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&dir);
    commit_dated(&dir, "main.txt", "main\n", "main", 1_699_999_000);
    git(&dir, &["checkout", "-q", "-b", "scrub-hist"]);
    commit_dated(&dir, "scrub.txt", "scrub-v1\n", "v1", 1_700_000_000);
    commit_dated(&dir, "scrub.txt", "scrub-v2\n", "v2", 1_700_001_000);
    commit_dated(&dir, "scrub.txt", "scrub-v3\n", "v3", 1_700_002_000);
    git(&dir, &["checkout", "-q", "main"]);

    let (_tmp, base) = boot_with_repo("fixture", &dir).await;
    let client = reqwest::Client::new();
    let page: serde_json::Value = client
        .get(format!("{base}/api/file/stops"))
        .query(&[
            ("repo", "fixture"),
            ("path", "scrub.txt"),
            ("ref", "scrub-hist"),
        ])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let subjects: Vec<_> = page["stops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|stop| stop["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, ["v3", "v2", "v1"]);
    assert_eq!(page["total"], 3);
    assert_eq!(page["floor"]["when"], 1_700_000_000);

    let prior: serde_json::Value = client
        .get(format!("{base}/api/file/at"))
        .query(&[
            ("repo", "fixture"),
            ("path", "scrub.txt"),
            ("ref", "scrub-hist"),
            ("at", "1700001999"),
        ])
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(prior["content"], "scrub-v2\n");
    assert_eq!(prior["resolution"], "nearest-prior");
    assert_eq!(prior["floor"], page["floor"]);

    for endpoint in ["stops", "at"] {
        let invalid = client
            .get(format!("{base}/api/file/{endpoint}"))
            .query(&[
                ("repo", "fixture"),
                ("path", "scrub.txt"),
                ("ref", "--all"),
                ("at", "1700001999"),
            ])
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), 400);
    }
}
