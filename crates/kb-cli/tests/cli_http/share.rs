//! `kb share` integration tests. The verb is HTTP-only; the daemon-backed
//! tests boot a real daemon (mirroring `download.rs`) but exercise only the
//! no-network paths of the route (empty list; validation 400) — a real
//! deploy needs Cloudflare/GitHub credentials and lives in the engine's
//! `#[ignore]` lane. The rest are help / clean-failure checks.
use crate::common::canon_dir;

use assert_cmd::Command;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::Duration;

async fn boot() -> (tempfile::TempDir, String) {
    boot_with(|source| {
        std::fs::copy(
            canon_dir().join("fullscreen-viz.html"),
            source.join("fullscreen-viz.html"),
        )
        .unwrap();
    })
    .await
}

/// Boot a daemon over a fresh `smoke` kb, letting `setup` populate the corpus
/// source dir first.
async fn boot_with(setup: impl FnOnce(&std::path::Path)) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    setup(&source);

    let daemon_name = format!(
        "cli-share-{}",
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
    tokio::time::sleep(Duration::from_millis(900)).await;
    (tmp, format!("http://{addr}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_list_empty_via_daemon() {
    let (_tmp, url) = boot().await;
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["share", "list", "--kb", "smoke", "--json", "--daemon", &url])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&out).expect("json array");
    assert_eq!(parsed.as_array().map(|a| a.len()), Some(0), "no shares yet");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_create_without_gate_or_public_fails_with_message() {
    let (_tmp, url) = boot().await;
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "share",
            "fullscreen-viz.html",
            "--kb",
            "smoke",
            "--host",
            "cloudflare-pages",
            "--daemon",
            &url,
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("--gate") || text.contains("--public"),
        "expected a gate/public hint, got: {text}"
    );
}

// invariant:9 local-export
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn share_local_bundle_zip_and_dir_relativize_links() {
    let (_tmp, url) = boot_with(|src| {
        let bundle = src.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        // `a.html` links to its sibling via the SPA-permalink form — which only
        // works against the live daemon. The local bundle must relativize it.
        std::fs::write(
            bundle.join("a.html"),
            r#"<!doctype html><html><body><a href="/a/smoke/bundle/b.html">b</a></body></html>"#,
        )
        .unwrap();
        std::fs::write(bundle.join("b.html"), "<!doctype html><h1>B</h1>").unwrap();
    })
    .await;

    let work = tempfile::tempdir().unwrap();

    // `--local PATH.zip` writes a (non-empty) zip.
    let zip_path = work.path().join("out.zip");
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "share",
            "bundle",
            "--kb",
            "smoke",
            "--local",
            zip_path.to_str().unwrap(),
            "--daemon",
            &url,
        ])
        .assert()
        .success();
    assert!(zip_path.is_file(), "zip written");
    assert!(
        std::fs::metadata(&zip_path).unwrap().len() > 0,
        "zip non-empty"
    );

    // `--local DIR` extracts a self-contained bundle with relativized links.
    let out_dir = work.path().join("out");
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "share",
            "bundle",
            "--kb",
            "smoke",
            "--local",
            out_dir.to_str().unwrap(),
            "--daemon",
            &url,
        ])
        .assert()
        .success();
    let a = std::fs::read_to_string(out_dir.join("a.html")).expect("a.html extracted");
    assert!(
        a.contains(r#"href="b.html""#),
        "in-share permalink relativized: {a}"
    );
    assert!(!a.contains("/a/smoke/"), "no daemon permalink remains: {a}");
    assert!(out_dir.join("b.html").is_file(), "sibling extracted");
}

// ---------- lightweight (no daemon) ----------

#[test]
fn share_help_lists_flags() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["share", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    for needle in [
        "--gate",
        "--public",
        "--host",
        "--links",
        "--update",
        "--no-scrub",
        "--local",
    ] {
        assert!(text.contains(needle), "help missing {needle}: {text}");
    }
}

#[test]
fn share_revoke_help_works() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["share", "revoke", "--help"])
        .assert()
        .success();
}

#[test]
fn share_with_no_target_or_subcommand_errors() {
    // target None + action None → the dispatch errors before any network.
    Command::cargo_bin("kb")
        .unwrap()
        .args(["share"])
        .assert()
        .failure();
}

#[test]
fn share_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "share",
            "x.html",
            "--kb",
            "smoke",
            "--public",
            "--host",
            "github-pages",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}
