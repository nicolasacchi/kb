//! V3.4-C1 — behavioral time-series HTTP tests.
//!
//! Fixture: commits at known author dates across 3 ISO weeks → expected
//! buckets; path scoping. Dates are anchored relative to "now" so the
//! weeks=104 cap always includes them. Pure git walk (no store counters) —
//! still poll `/api/repos` so the repo is registered before asserting.

use crate::common::git;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn write_file(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn commit_dated(
    dir: &Path,
    email: &str,
    name: &str,
    message: &str,
    date: &str,
    files: &[(&str, &str)],
) {
    git(dir, &["config", "user.email", email]);
    git(dir, &["config", "user.name", name]);
    for (rel, contents) in files {
        write_file(dir, rel, contents);
    }
    git(dir, &["add", "-A"]);
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("git commit");
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Monday 00:00 UTC of the ISO week containing `unix` (same pure math as
/// `behavioral::iso_week_start_unix`).
fn iso_week_start_unix(unix: i64) -> i64 {
    let days = unix.div_euclid(86_400);
    let weekday = (days + 3).rem_euclid(7);
    (days - weekday) * 86_400
}

/// Three ISO weeks ending near "today" so weeks=26/104 always covers them.
/// Returns (tmp, dir, w1_start, w2_start, w3_start).
fn build_fixture() -> (tempfile::TempDir, std::path::PathBuf, i64, i64, i64) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let this_mon = iso_week_start_unix(now);
    // Use three completed weeks so wall-clock "this week" doesn't race a
    // commit dated mid-week against a just-rolled Monday.
    let w3 = this_mon - 7 * 86_400;
    let w2 = this_mon - 14 * 86_400;
    let w1 = this_mon - 21 * 86_400;

    let fmt = |unix: i64| {
        // git wants "YYYY-MM-DDTHH:MM:SS +0000"
        let days = unix / 86_400;
        // Use a fixed offset from epoch via chrono-less formatting: shell date.
        let out = Command::new("date")
            .args(["-u", "-d", &format!("@{unix}"), "+%Y-%m-%dT%H:%M:%S +0000"])
            .output()
            .expect("date");
        assert!(out.status.success(), "date failed for {unix}");
        let _ = days;
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "alice@example.com"]);
    git(&dir, &["config", "user.name", "Alice"]);

    // Week 1: alice + bob touch hot.rs; alice also pair
    commit_dated(
        &dir,
        "alice@example.com",
        "Alice",
        "w1 alice",
        &fmt(w1 + 12 * 3600),
        &[("hot.rs", "fn a() {}\n"), ("pair.rs", "fn p() {}\n")],
    );
    commit_dated(
        &dir,
        "bob@example.com",
        "Bob",
        "w1 bob",
        &fmt(w1 + 2 * 86_400 + 12 * 3600),
        &[("hot.rs", "fn a() { 1 }\n")],
    );
    // Week 2: alice only
    commit_dated(
        &dir,
        "alice@example.com",
        "Alice",
        "w2 alice",
        &fmt(w2 + 2 * 86_400 + 12 * 3600),
        &[("hot.rs", "fn a() { 2 }\n")],
    );
    // Week 3: carol on cold.rs only
    commit_dated(
        &dir,
        "carol@example.com",
        "Carol",
        "w3 carol",
        &fmt(w3 + 86_400 + 12 * 3600),
        &[("cold.rs", "fn c() {}\n")],
    );

    (tmp, dir, w1, w2, w3)
}

async fn boot(repo_name: &str, repo_dir: &Path) -> (tempfile::TempDir, String) {
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
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

/// Poll until the repo appears in `/api/repos` (git-derived routes still
/// need the boot-time repo registration to be visible).
async fn wait_for_repo(base: &str, repo: &str) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if body["repos"]
                    .as_array()
                    .map(|repos| repos.iter().any(|r| r["name"] == repo))
                    .unwrap_or(false)
                {
                    return;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "repo {repo:?} never appeared in /api/repos"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeseries_three_weeks_and_path_scope() {
    let (_repo_tmp, dir, w1, w2, w3) = build_fixture();
    let (_daemon_tmp, base) = boot("ts", &dir).await;
    wait_for_repo(&base, "ts").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/behavioral/timeseries"))
        .query(&[("repo", "ts"), ("weeks", "26")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "behavioral/1");
    assert_eq!(body["repo"], "ts");
    assert_eq!(body["weeks"], 26);
    assert_eq!(body["truncated"], false);
    let note = body["note"].as_str().unwrap_or("");
    assert!(
        !note.is_empty()
            && (note.contains("Activity")
                || note.contains("activity")
                || note.contains("Attention")
                || note.contains("attention")),
        "note must be an activity/attention signal, got: {note}"
    );

    let buckets = body["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 3, "expected 3 week buckets, got {buckets:?}");
    // Ascending week order
    assert_eq!(buckets[0]["week_start_unix"], w1);
    assert_eq!(buckets[0]["commits"], 2);
    assert_eq!(buckets[0]["authors"], 2);
    assert!(buckets[0]["churn"].as_i64().unwrap() > 0);

    assert_eq!(buckets[1]["week_start_unix"], w2);
    assert_eq!(buckets[1]["commits"], 1);
    assert_eq!(buckets[1]["authors"], 1);

    assert_eq!(buckets[2]["week_start_unix"], w3);
    assert_eq!(buckets[2]["commits"], 1);
    assert_eq!(buckets[2]["authors"], 1);

    // Path scope: hot.rs — w3 (cold.rs only) drops out
    let resp = client
        .get(format!("{base}/api/behavioral/timeseries"))
        .query(&[("repo", "ts"), ("path", "hot.rs"), ("weeks", "26")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let scoped: serde_json::Value = resp.json().await.unwrap();
    let sb = scoped["buckets"].as_array().unwrap();
    assert_eq!(sb.len(), 2, "hot.rs should hit w1+w2 only: {sb:?}");
    assert_eq!(sb[0]["week_start_unix"], w1);
    assert_eq!(sb[0]["commits"], 2);
    assert_eq!(sb[1]["week_start_unix"], w2);
    assert_eq!(sb[1]["commits"], 1);
    assert_eq!(scoped["path"], "hot.rs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeseries_empty_window_is_empty_buckets_not_error() {
    let (_repo_tmp, dir, _w1, _w2, _w3) = build_fixture();
    let (_daemon_tmp, base) = boot("ts", &dir).await;
    wait_for_repo(&base, "ts").await;
    let client = reqwest::Client::new();

    // Path that nothing ever touched → empty buckets + explanatory note.
    let resp = client
        .get(format!("{base}/api/behavioral/timeseries"))
        .query(&[
            ("repo", "ts"),
            ("path", "never-touched.rs"),
            ("weeks", "26"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["buckets"].as_array().unwrap().len(), 0);
    assert!(
        body["note"]
            .as_str()
            .unwrap()
            .contains("No commit activity")
            || body["note"].as_str().unwrap().contains("since_unix"),
        "empty window must explain itself: {}",
        body["note"]
    );
    assert_eq!(body["truncated"], false);
}
