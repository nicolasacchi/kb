//! `kb-code search transcripts` + `kb-code transcripts status` — W2.5 CLI
//! smoke tests against a REAL running kb-code daemon. Mirrors
//! `tests/search.rs`'s own `boot()`/`wait_for_json` conventions, but seeds
//! a `[transcripts] root` fixture directory instead of a git repo.

use crate::common::wait_for_json;
use assert_cmd::Command;
use kb_code_server::config::{KbCodeConfig, TranscriptsSection};
use predicates::str::contains;
use std::path::Path;

fn seed_transcript_root(root: &Path) {
    let proj = root.join("-fixture-project");
    std::fs::create_dir_all(&proj).unwrap();
    let line = r#"{"type":"user","uuid":"u1","parentUuid":null,"sessionId":"s-fixture","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{"role":"user","content":"a very unusual gorgonzola marker string"}}"#;
    std::fs::write(proj.join("s-fixture.jsonl"), format!("{line}\n")).unwrap();
}

async fn boot(
    transcripts_root: &Path,
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        transcripts: TranscriptsSection {
            enabled: true,
            root: transcripts_root.to_string_lossy().into_owned(),
            exclude_projects: Vec::new(),
            index_thinking: true,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_transcripts_finds_the_seeded_turn() {
    let root_tmp = tempfile::tempdir().unwrap();
    seed_transcript_root(root_tmp.path());
    let (_tmp, url, task) = boot(root_tmp.path()).await;

    wait_for_json(
        &format!("{url}/api/transcripts/status"),
        std::time::Duration::from_secs(15),
        |b| b["turns"].as_u64().unwrap_or(0) > 0,
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["search", "transcripts", "gorgonzola", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("s-fixture"))
        .stdout(contains("gorgonzola"));

    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search",
            "transcripts",
            "gorgonzola",
            "--daemon",
            &url,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON stdout");
    assert!(body["hits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["session_id"] == "s-fixture"));
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn transcripts_status_reports_files_and_turns() {
    let root_tmp = tempfile::tempdir().unwrap();
    seed_transcript_root(root_tmp.path());
    let (_tmp, url, task) = boot(root_tmp.path()).await;

    wait_for_json(
        &format!("{url}/api/transcripts/status"),
        std::time::Duration::from_secs(15),
        |b| b["turns"].as_u64().unwrap_or(0) > 0,
    )
    .await;

    Command::cargo_bin("kb-code")
        .unwrap()
        .args(["transcripts", "status", "--daemon", &url])
        .assert()
        .success()
        .stdout(contains("enabled: true"))
        .stdout(contains("files:   1"))
        .stdout(contains("turns:   1"));
    task.abort();
}

#[test]
fn search_transcripts_unreachable_daemon_fails_cleanly() {
    Command::cargo_bin("kb-code")
        .unwrap()
        .args([
            "search",
            "transcripts",
            "anything",
            "--daemon",
            "http://127.0.0.1:0",
        ])
        .assert()
        .failure()
        .stderr(contains("is kb-code-server running"));
}
