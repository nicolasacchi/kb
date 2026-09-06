//! P3 — `kb search --json` success-path coverage against a live
//! daemon. The existing `json_output.rs` only covers verbs that work
//! without one (`status --json`, `comments list --json` on empty
//! configs). Search runs through the daemon's HTTP API, so it needs
//! a real one in-process.
//!
//! Error-path note: `kb search` on a daemon that fails returns
//! `anyhow::Error` which prints plain text to stderr + exits non-zero.
//! `--json` consumers get non-JSON on failure — a latent issue the
//! deep review flagged. This file pins the current behavior in
//! `search_json_error_is_plain_text_not_json` (documentation), with a
//! note that emitting JSON-on-stderr-when-`--json`-is-set is a
//! contract decision for a follow-up.
use crate::common::canon_dir;

use assert_cmd::Command;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::Duration;

/// Boot a real daemon on a random port, indexing the canon corpus
/// under a per-test tempdir. Mirrors `crates/kb-server/tests/end_to_end.rs::boot`
/// — kept duplicated rather than imported because Cargo doesn't share
/// integration-test fixtures across crates.
async fn boot() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    for name in [
        "fullscreen-viz.html",
        "kitchen-sink.html",
        "multi-page.html",
        "cost-of-abstraction.html",
    ] {
        std::fs::copy(canon_dir().join(name), source.join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }

    let daemon_name = format!(
        "cli-json-{}",
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
    // The indexer needs a beat to walk the initial corpus before BM25
    // hits are queryable. Same 800ms grace the kb-server e2e uses.
    tokio::time::sleep(Duration::from_millis(800)).await;
    (tmp, addr)
}

// Multi-thread runtime: the test thread blocks on assert_cmd's sync
// subprocess wait, so a single-threaded runtime stalls the daemon
// task. With 2 workers the daemon keeps accepting connections from
// the subprocess.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn search_json_success_emits_hits_array_with_id_title_path() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "search", "borrow", // matches "Visualizing the Borrow Checker" in canon
            "--daemon", &url, "--mode", "keyword", "--kb", "smoke", "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(out).expect("stdout is utf-8");
    let value: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("expected JSON; got:\n{text}\nerror: {e}"));

    // Shape: { "hits": [...], "ms": <u64>, "source": "<daemon>" }
    let hits = value["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("'hits' must be an array; got: {value}"));
    assert!(
        !hits.is_empty(),
        "at least one hit for 'borrow' in canon; got: {value}"
    );
    for h in hits {
        assert!(h["id"].is_string(), "each hit has a string id; got: {h}");
        assert!(
            h["title"].is_string(),
            "each hit has a string title; got: {h}"
        );
        assert!(
            h["path"].is_string(),
            "each hit has a string path; got: {h}"
        );
    }
    assert!(
        value["ms"].is_number(),
        "'ms' must be a number; got: {value}"
    );
    assert!(
        value["source"].as_str() == Some(&url),
        "'source' must echo the daemon URL; got: {value}"
    );
}

#[tokio::test]
async fn search_json_error_emits_json_envelope_on_stdout() {
    // Deep-review M-cli: `kb search --json` against an unreachable
    // daemon must emit a JSON `{error, source}` envelope on stdout so
    // `jq` pipelines don't break. Pre-fix this printed a plain-text
    // anyhow message via stderr; consumers had to special-case.
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "search",
            "anything",
            "--daemon",
            "http://127.0.0.1:1", // RFC reserved + unlikely to bind
            "--mode",
            "keyword",
            "--kb",
            "smoke",
            "--json",
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json error must be JSON; got: {stdout:?} ({e})"));
    assert!(
        value["error"].as_str().is_some(),
        "envelope needs an 'error' field; got: {value}"
    );
    assert!(
        value["source"].as_str().is_some(),
        "envelope needs a 'source' field; got: {value}"
    );
}
