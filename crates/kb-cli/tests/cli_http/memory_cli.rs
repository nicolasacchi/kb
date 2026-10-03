//! M5 — `kb remember` / `kb recall` / `kb forget` against a live daemon.
//! Boots an in-process daemon with two memory corpora (global + project,
//! no embedder → BM25) and drives the agent-facing loop through the
//! actual `kb` binary via assert_cmd.

use assert_cmd::Command;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

async fn boot_memory() -> (tempfile::TempDir, SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let gdir = tmp.path().join("gmem");
    let pdir = tmp.path().join("pmem");
    std::fs::create_dir_all(&gdir).unwrap();
    std::fs::create_dir_all(&pdir).unwrap();

    let daemon_name = format!(
        "cli-mem-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("gmem").unwrap(),
        KbSection {
            path: gdir,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
            memory_scope: Some("global".into()),
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
    kb_map.insert(
        KbName::new("pmem").unwrap(),
        KbSection {
            path: pdir,
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: std::collections::BTreeMap::new(),
            memory_scope: Some("project".into()),
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
    tokio::time::sleep(Duration::from_millis(400)).await;
    (tmp, addr)
}

async fn wait_recall_includes(client: &reqwest::Client, addr: SocketAddr, q: &str, want_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let v: serde_json::Value = client
            .get(format!(
                "http://{addr}/api/memory/recall?q={q}&scope=all&limit=20"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        let found = v["hits"]
            .as_array()
            .map(|a| a.iter().any(|h| h["id"].as_str() == Some(want_id)))
            .unwrap_or(false);
        if found {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for recall to include {want_id}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remember_recall_forget_round_trip() {
    let (_tmp, addr) = boot_memory().await;
    let url = format!("http://{addr}");

    // remember → resolves --scope global to the "gmem" corpus.
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "remember",
            "user prefers dark mode zigzag",
            "--scope",
            "global",
            "--salience",
            "0.9",
            "--daemon",
            &url,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    let created: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("remember --json: {text}\n{e}"));
    let id = created["id"].as_str().expect("id").to_string();
    assert!(created["path"].as_str().unwrap().ends_with(".html"));

    // The watcher indexes the write-only file → recall surfaces it.
    let client = reqwest::Client::new();
    wait_recall_includes(&client, addr, "zigzag", &id).await;

    // recall --json across all corpora must include the remembered id.
    let rout = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "recall", "zigzag", "--scope", "all", "--daemon", &url, "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rtext = String::from_utf8(rout).unwrap();
    let recalled: serde_json::Value =
        serde_json::from_str(&rtext).unwrap_or_else(|e| panic!("recall --json: {rtext}\n{e}"));
    let hits = recalled["hits"].as_array().expect("hits array");
    assert!(
        hits.iter().any(|h| h["id"].as_str() == Some(id.as_str())),
        "recall includes the remembered id: {rtext}"
    );
    assert_eq!(recalled["source"].as_str(), Some(url.as_str()));

    // forget removes it.
    Command::cargo_bin("kb")
        .unwrap()
        .args(["forget", &id, "--kb", "gmem", "--daemon", &url])
        .assert()
        .success();
}

/// v044-X3 — a `kb remember` against a daemon that is not there is QUEUED
/// (exit 0, `queued: true`, a client_ref) instead of lost; `kb outbox flush`
/// against the live daemon replays it with the same client_ref, the memory is
/// recallable exactly once, and the spool is empty afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remember_to_a_dead_daemon_is_queued_and_flush_replays_it_once() {
    let (_tmp, addr) = boot_memory().await;
    let url = format!("http://{addr}");
    let cache = tempfile::tempdir().unwrap();
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };

    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("XDG_CACHE_HOME", cache.path())
        .args([
            "remember",
            "the egret waits at the weir quillon",
            "--kb",
            "gmem",
            "--no-session",
            "--daemon",
            &dead,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let queued: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(queued["queued"], true, "{queued}");
    let client_ref = queued["client_ref"].as_str().unwrap().to_string();
    let spool = cache.path().join("kb/outbox");
    assert!(
        spool.join(format!("{client_ref}.json")).is_file(),
        "the intent must be on disk"
    );

    let flushed = Command::cargo_bin("kb")
        .unwrap()
        .env("XDG_CACHE_HOME", cache.path())
        .args(["outbox", "flush", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let flushed: serde_json::Value = serde_json::from_slice(&flushed).unwrap();
    assert_eq!(flushed["before"], 1, "{flushed}");
    assert_eq!(flushed["after"], 0, "{flushed}");
    assert!(
        !spool.join(format!("{client_ref}.json")).exists(),
        "a replayed entry leaves the spool"
    );

    // Recallable, and exactly once.
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let v: serde_json::Value = client
            .get(format!(
                "http://{addr}/api/memory/recall?q=quillon&scope=all&limit=20"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        let n = v["hits"].as_array().map(|a| a.len()).unwrap_or(0);
        if n >= 1 {
            assert_eq!(n, 1, "replay must not duplicate the memory: {v}");
            break;
        }
        assert!(Instant::now() < deadline, "replayed memory never recalled");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// v0.44 X6 - `kb remember --wait` against a dead daemon exits non-zero and
/// spools NOTHING (the default exits 0 with `queued`, which carries no id).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remember_wait_against_a_dead_daemon_fails_and_spools_nothing() {
    let cache = tempfile::tempdir().unwrap();
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    Command::cargo_bin("kb")
        .unwrap()
        .env("XDG_CACHE_HOME", cache.path())
        .args([
            "remember",
            "the bittern holds still among the reeds",
            "--kb",
            "gmem",
            "--no-session",
            "--wait",
            "--daemon",
            &dead,
        ])
        .assert()
        .failure();
    let spool = cache.path().join("kb/outbox");
    let n = std::fs::read_dir(&spool).map(|d| d.count()).unwrap_or(0);
    assert_eq!(n, 0, "--wait must not queue anything");
}

#[tokio::test]
async fn recall_json_error_emits_envelope_on_stdout() {
    // Unreachable daemon → `--json` must still emit a {error, source}
    // envelope on stdout so jq pipelines parse cleanly.
    let assert = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "recall",
            "anything",
            "--daemon",
            "http://127.0.0.1:1",
            "--json",
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json error must be JSON; got: {stdout:?} ({e})"));
    assert!(v["error"].as_str().is_some(), "envelope needs error: {v}");
    assert!(v["source"].as_str().is_some(), "envelope needs source: {v}");
}
