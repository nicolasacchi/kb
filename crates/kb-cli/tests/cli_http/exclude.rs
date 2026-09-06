//! `kb exclude` / `kb pause` / `kb resume` integration tests — X3 (v0.24).
//!
//! Boot pattern mirrors `cat_read.rs` (real daemon, tempdir corpus, random
//! port). Pins the CLI mirror of the exclusion + pause routes:
//! - exclude by unique filename → doc drops from the index, file untouched;
//! - `--list` (human + `--json`) shows the row;
//! - `--rm` resolves against the exclusion list (the index no longer knows
//!   the file) and brings the doc back;
//! - a path-shaped target that matches no indexed artifact is excluded
//!   verbatim (pre-emptive exclusion) with a stderr note;
//! - `kb pause`/`kb resume` flip the source's (now-enforced, D6) flag.
use crate::common::{base_config, canon_dir, kb_section};

use assert_cmd::Command;
use kb_core::config::KbSection;
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// Boot a daemon over a tempdir corpus (two canon files). Returns
/// `(tempdir, source, base_url)`.
async fn boot() -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for name in ["kitchen-sink.html", "multi-page.html"] {
        std::fs::copy(canon_dir().join(name), source.join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    let daemon_name = format!(
        "cli-excl-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source.clone()));
    let cfg = base_config(&daemon_name, kb_map);
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(900)).await;
    (tmp, source, format!("http://{addr}"))
}

async fn doc_ids(url: &str) -> Vec<String> {
    let body: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!("{url}/api/kb/smoke/docs?limit=100"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    body.iter()
        .filter_map(|d| d["id"].as_str().map(str::to_string))
        .collect()
}

/// Poll the docs list until `pred` holds (exclude/re-include ride the
/// async ingest channel).
async fn wait_for_docs(url: &str, what: &str, pred: impl Fn(&[String]) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let ids = doc_ids(url).await;
        if pred(&ids) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}; docs = {ids:?}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn kb_cmd(url: &str, args: &[&str]) -> Command {
    let mut cmd = Command::cargo_bin("kb").unwrap();
    cmd.args(args).args(["--kb", "smoke", "--daemon", url]);
    cmd
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclude_list_and_rm_roundtrip() {
    let (_tmp, source, url) = boot().await;
    wait_for_docs(&url, "initial index", |ids| ids.len() == 2).await;

    // Exclude by unique filename suffix.
    let out = kb_cmd(&url, &["exclude", "kitchen-sink.html"])
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("excluded kitchen-sink.html"),
        "confirmation names the path: {text}"
    );
    wait_for_docs(&url, "exclusion cascade", |ids| ids.len() == 1).await;
    assert!(
        source.join("kitchen-sink.html").exists(),
        "exclusion never touches the source file"
    );

    // --list human output names the path.
    let out = kb_cmd(&url, &["exclude", "--list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("kitchen-sink.html") && text.contains("1 exclusion"),
        "list output: {text}"
    );

    // --list --json is the raw daemon array.
    let out = kb_cmd(&url, &["exclude", "--list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).expect("--json emits JSON");
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"].as_str(), Some("kitchen-sink.html"));
    assert_eq!(rows[0]["present_on_disk"].as_bool(), Some(true));
    assert_eq!(
        rows[0]["artifact_id"].as_str().map(str::len),
        Some(12),
        "list rows carry the derived 12-hex artifact id"
    );

    // --rm resolves against the exclusion list (basename form) and brings
    // the doc back.
    let out = kb_cmd(&url, &["exclude", "kitchen-sink.html", "--rm"])
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("re-included kitchen-sink.html"),
        "rm confirmation: {text}"
    );
    wait_for_docs(&url, "re-include reindex", |ids| ids.len() == 2).await;

    let out = kb_cmd(&url, &["exclude", "--list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&out).contains("no exclusions"),
        "list is empty again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclude_unindexed_path_is_preemptive_with_note() {
    let (_tmp, _source, url) = boot().await;
    wait_for_docs(&url, "initial index", |ids| ids.len() == 2).await;

    // A path-shaped target that matches nothing indexed → excluded
    // verbatim, with a stderr note about the literal form.
    let out = kb_cmd(
        &url,
        &["exclude", "ghost/never.html", "--note", "broken generator"],
    )
    .assert()
    .success()
    .get_output()
    .stderr
    .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("matched no indexed artifact"),
        "pre-emptive exclusion warns: {text}"
    );

    let out = kb_cmd(&url, &["exclude", "--list", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows[0]["path"].as_str(), Some("ghost/never.html"));
    assert_eq!(rows[0]["note"].as_str(), Some("broken generator"));
    assert_eq!(rows[0]["present_on_disk"].as_bool(), Some(false));

    // The indexed corpus is untouched.
    assert_eq!(doc_ids(&url).await.len(), 2);

    // Cleanup path: --rm on the exact rel path.
    kb_cmd(&url, &["exclude", "ghost/never.html", "--rm"])
        .assert()
        .success();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exclude_refuses_junk_targets_cleanly() {
    let (_tmp, _source, url) = boot().await;
    wait_for_docs(&url, "initial index", |ids| ids.len() == 2).await;

    // Not path-shaped + not indexed → refuse (typo'd id/title, not a path).
    let out = kb_cmd(&url, &["exclude", "totallybogus"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("matched no artifact"),
        "junk target error: {text}"
    );

    // --rm on something never excluded → clean error pointing at --list.
    let out = kb_cmd(&url, &["exclude", "nope.html", "--rm"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(text.contains("is not excluded"), "rm-miss error: {text}");

    // No target and no --list → usage error, not a silent success.
    let out = kb_cmd(&url, &["exclude"])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("--list"),
        "bare `kb exclude` points at --list: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pause_and_resume_flip_the_source_flag() {
    let (_tmp, _source, url) = boot().await;

    async fn paused_flag(url: &str) -> bool {
        let sources: Vec<serde_json::Value> = reqwest::Client::new()
            .get(format!("{url}/api/kb/smoke/sources"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        sources[0]["paused"].as_bool().unwrap()
    }
    assert!(!paused_flag(&url).await, "sources start unpaused");

    let out = kb_cmd(&url, &["pause"])
        .assert()
        .success()
        .get_output()
        .stderr
        .clone();
    assert!(
        String::from_utf8_lossy(&out).contains("paused source"),
        "pause confirmation"
    );
    assert!(paused_flag(&url).await, "pause flipped the flag");

    // --json emits the raw daemon response.
    let out = kb_cmd(&url, &["resume", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("--json emits JSON");
    assert_eq!(body["paused"].as_bool(), Some(false));
    assert!(!paused_flag(&url).await, "resume flipped it back");
}
