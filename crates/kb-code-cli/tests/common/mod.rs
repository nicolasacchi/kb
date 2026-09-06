//! Shared helpers for kb-code-cli integration tests.
//!
//! Only byte-identical copies live here. Divergent `fixture_repo` /
//! `boot` / `wait_for_indexed` implementations stay next to the tests
//! that own them.

#![allow(dead_code)]

use kb_code_server::config::{KbCodeConfig, RepoEntry};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git -C {} {:?} failed: {}",
        dir.display(),
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

pub async fn boot(
    repo_dir: &Path,
    repo_name: &str,
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

pub async fn wait_for_json(
    url: &str,
    timeout: Duration,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + timeout;
    loop {
        let body: serde_json::Value = client.get(url).send().await.unwrap().json().await.unwrap();
        if pred(&body) || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
