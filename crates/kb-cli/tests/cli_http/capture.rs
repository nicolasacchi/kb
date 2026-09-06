//! `kb capture` integration tests (U3, v0.25 quick capture). Boots a real
//! daemon (mirrors `notes.rs`/`comments.rs`) and drives the CLI verb end to
//! end: a file capture, a stdin capture, a url/text stub, `--output json`,
//! the 415 unsupported-extension problem+json surfaced as a clean CLI
//! error, and the local pre-flight validation (no files/url/text) that
//! never touches the network.
use crate::common::canon_dir;

use assert_cmd::Command;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::Duration;

/// Boot a daemon on a random port indexing the canon corpus under a per-test
/// tempdir. Mirrors `notes.rs::boot` (Cargo doesn't share fixtures across
/// integration-test files).
async fn boot() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for name in ["fullscreen-viz.html", "kitchen-sink.html"] {
        std::fs::copy(canon_dir().join(name), source.join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    let daemon_name = format!(
        "cli-capture-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: source,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
            memory_scope: None,
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: None,
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            reconcile_secs: None,
            capture_dir: None,
            resurface: None,
            slo: None,
        },
    );
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        server: ServerSection::default(),
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
    };
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    (tmp, format!("http://{addr}"))
}

/// Run `kb capture …` against `url`, asserting success; returns stdout.
fn run_ok(url: &str, args: &[&str]) -> String {
    let mut full = vec!["capture"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["--daemon", url]);
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(&full)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_file_prints_id_relative_and_artifact_url() {
    let (tmp, url) = boot().await;
    let src = tmp.path().join("note.md");
    std::fs::write(&src, "# Quick note\n\nsome body").unwrap();

    let out = run_ok(
        &url,
        &[
            src.to_str().unwrap(),
            "--kb",
            "smoke",
            "--title",
            "Quick note",
            "--tags",
            "a,b",
        ],
    );
    assert!(out.contains("✓ captured "), "got: {out}");
    assert!(out.contains("capture/"), "got: {out}");
    // Second line is the full artifact URL against the daemon base.
    let rel_line = out
        .lines()
        .find(|l| l.trim_start().starts_with(&url))
        .unwrap_or_else(|| panic!("no artifact URL line in: {out}"));
    assert!(
        rel_line.contains("/a/smoke/"),
        "artifact URL missing /a/smoke/: {rel_line}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_stdin_uses_name_stem_and_writes_md() {
    let (_tmp, url) = boot().await;
    let mut full = vec!["capture", "-", "--name", "meeting-notes", "--kb", "smoke"];
    full.push("--daemon");
    full.push(&url);
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(&full)
        .write_stdin("# Meeting notes\n\n- decided X")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("meeting-notes"),
        "expected the stdin filename stem in the source_relative: {out}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_url_text_stub_with_no_files() {
    let (_tmp, url) = boot().await;
    let out = run_ok(
        &url,
        &[
            "--url",
            "https://example.com/article",
            "--text",
            "shared snippet",
            "--title",
            "Article",
            "--kb",
            "smoke",
        ],
    );
    assert!(out.contains("✓ captured "), "got: {out}");
    assert!(out.ends_with(".md\n") || out.contains(".md"), "got: {out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_output_json_emits_raw_response() {
    let (tmp, url) = boot().await;
    let src = tmp.path().join("json-check.md");
    std::fs::write(&src, "# json check").unwrap();

    let out = run_ok(
        &url,
        &[src.to_str().unwrap(), "--kb", "smoke", "--output", "json"],
    );
    let body: serde_json::Value =
        serde_json::from_str(&out).expect("--output json must be valid JSON");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1);
    assert!(items[0]["id"].as_str().is_some());
    assert!(items[0]["source_relative"]
        .as_str()
        .unwrap()
        .starts_with("capture/"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_unsupported_extension_surfaces_problem_detail() {
    let (tmp, url) = boot().await;
    let src = tmp.path().join("payload.exe");
    std::fs::write(&src, b"binary junk").unwrap();

    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "capture",
            src.to_str().unwrap(),
            "--kb",
            "smoke",
            "--daemon",
            &url,
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("indexable extension"),
        "expected the daemon's 415 detail surfaced on stderr, got: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_with_no_files_url_or_text_fails_locally_without_network() {
    // No --daemon at all — if this reached out over HTTP, it would either
    // hang on the (unreachable-in-CI) default 127.0.0.1:4000 or fail with a
    // connection error rather than the local validation message. Asserting
    // on the exact message pins that the empty-input guard runs first.
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args(["capture", "--kb", "smoke", "--daemon", "http://127.0.0.1:1"])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.contains("requires at least one FILE"),
        "got: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_rejects_dash_mixed_with_paths_locally() {
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "capture",
            "a.md",
            "-",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(assert.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("only FILES argument"), "got: {stderr}");
}
