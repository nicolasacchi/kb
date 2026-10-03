//! `kb search` scope: a bare search on a multi-kb daemon is federated.
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
    // Two corpora, disjoint content: "borrow" only matches `beta`.
    let mut sources = Vec::new();
    for (kb, files) in [
        ("alpha", vec!["multi-page.html"]),
        ("beta", vec!["fullscreen-viz.html"]),
    ] {
        let dir = tmp.path().join(kb);
        std::fs::create_dir_all(&dir).unwrap();
        for name in files {
            std::fs::copy(canon_dir().join(name), dir.join(name))
                .unwrap_or_else(|e| panic!("copy {name}: {e}"));
        }
        sources.push((kb, dir));
    }

    let daemon_name = format!(
        "cli-scope-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    for (kb, dir) in sources {
        kb_map.insert(
            KbName::new(kb).unwrap(),
            KbSection {
                path: dir,
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
                project_slugs: Vec::new(),
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
                id_patterns: Vec::new(),
            },
        );
    }
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

fn kb(args: &[&str], url: &str) -> std::process::Output {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "search", "borrow", "--daemon", url, "--mode", "keyword", "--json",
        ])
        .args(args)
        .output()
        .unwrap()
}

// Pinned to fail without the change: before, a multi-kb daemon answered
// a bare `kb search` with HTTP 400 "must specify ?kb=".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bare_search_on_a_two_kb_daemon_is_federated_and_says_so() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");
    let out = kb(&[], &url);
    assert!(
        out.status.success(),
        "bare search must not 400: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let hits = v["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "federated search finds the beta doc: {v}");
    assert!(
        hits.iter().all(|h| h["kb"] == "beta"),
        "federated hits carry their kb: {v}"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("searching all"), "stderr note: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kb_flag_pins_one_corpus_without_kb_field_or_note() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");
    let out = kb(&["--kb", "beta"], &url);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let hits = v["hits"].as_array().unwrap();
    assert!(!hits.is_empty(), "{v}");
    assert!(hits.iter().all(|h| h.get("kb").is_none()), "{v}");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("searching all"));

    let out = kb(&["--kb", "alpha"], &url);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["hits"].as_array().unwrap().is_empty(), "{v}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_scope_one_keeps_the_old_ambiguity_error() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");
    let out = kb(&["--scope", "one"], &url);
    assert!(!out.status.success(), "scope=one without --kb stays a 400");
    let out = kb(&["--scope", "all"], &url);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(!v["hits"].as_array().unwrap().is_empty(), "{v}");
}
