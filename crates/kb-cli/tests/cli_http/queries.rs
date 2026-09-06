//! GC-B3 — `kb queries --zero-hit` against a live daemon, plus W3 C-c's
//! `list`/`save`/`rm` CLI parity for the daemon-wide saved-query store
//! (`/api/saved-queries`). Mirrors the `search_json.rs` boot pattern
//! (Cargo doesn't share integration-test fixtures across crates, so the
//! fixture is duplicated rather than imported).
use crate::common::{canon_dir, kb_section};

use assert_cmd::Command;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::Duration;

/// Single-kb ("smoke") fixture — mirrors `search_json.rs::boot`.
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
        "cli-queries-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source));
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
    (tmp, addr)
}

/// Two-kb ("alpha"/"beta") fixture for `--scope all` fan-out.
async fn boot_two_kbs() -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let alpha_root = tmp.path().join("alpha-src");
    let beta_root = tmp.path().join("beta-src");
    std::fs::create_dir_all(&alpha_root).unwrap();
    std::fs::create_dir_all(&beta_root).unwrap();
    std::fs::write(
        alpha_root.join("alpha-note.html"),
        "<!doctype html><html><body><h1>alpha kb note</h1></body></html>",
    )
    .unwrap();
    std::fs::write(
        beta_root.join("beta-note.html"),
        "<!doctype html><html><body><h1>beta kb note</h1></body></html>",
    )
    .unwrap();

    let daemon_name = format!(
        "cli-queries-2kb-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("alpha").unwrap(), kb_section(alpha_root));
    kb_map.insert(KbName::new("beta").unwrap(), kb_section(beta_root));
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
    (tmp, addr)
}

async fn fire_search(addr: std::net::SocketAddr, q: &str, kb: &str) {
    let client = reqwest::Client::new();
    let _ = client
        .get(format!("http://{addr}/api/search"))
        .query(&[("q", q), ("mode", "keyword"), ("kb", kb)])
        .send()
        .await
        .unwrap();
}

// Multi-thread runtime: the test thread blocks on assert_cmd's sync
// subprocess wait (mirrors search_json.rs's rationale).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queries_zero_hit_json_groups_by_normalized_text() {
    let (_tmp, addr) = boot().await;
    // Three case/whitespace variants of the same zero-hit query.
    for q in ["Nomatchxyz", "nomatchxyz", "  nomatchxyz  "] {
        fire_search(addr, q, "smoke").await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    let url = format!("http://{addr}");
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "queries",
            "--zero-hit",
            "--kb",
            "smoke",
            "--daemon",
            &url,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(out).expect("stdout is utf-8");
    let groups: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("expected JSON; got:\n{text}\nerror: {e}"));
    let groups = groups.as_array().unwrap();
    assert_eq!(groups.len(), 1, "expected one normalized group: {groups:?}");
    assert_eq!(groups[0]["query"].as_str().unwrap(), "nomatchxyz");
    assert_eq!(groups[0]["count"].as_u64().unwrap(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queries_zero_hit_missing_kb_errors_for_scope_one() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");
    Command::cargo_bin("kb")
        .unwrap()
        .args(["queries", "--zero-hit", "--daemon", &url])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--kb is required"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queries_without_zero_hit_flag_errors() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");
    Command::cargo_bin("kb")
        .unwrap()
        .args(["queries", "--kb", "smoke", "--daemon", &url])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--zero-hit"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queries_zero_hit_scope_all_fans_out_across_kbs() {
    let (_tmp, addr) = boot_two_kbs().await;
    fire_search(addr, "florbnix", "alpha").await;
    fire_search(addr, "quixotropic", "beta").await;
    fire_search(addr, "quixotropic", "beta").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let url = format!("http://{addr}");
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "queries",
            "--zero-hit",
            "--scope",
            "all",
            "--daemon",
            &url,
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let text = String::from_utf8(out).expect("stdout is utf-8");
    let rows: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("expected JSON; got:\n{text}\nerror: {e}"));
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "expected one row per kb: {rows:?}");
    assert_eq!(rows[0]["kb"].as_str().unwrap(), "alpha");
    assert_eq!(rows[1]["kb"].as_str().unwrap(), "beta");
    assert_eq!(rows[0]["groups"][0]["query"].as_str().unwrap(), "florbnix");
    assert_eq!(
        rows[1]["groups"][0]["query"].as_str().unwrap(),
        "quixotropic"
    );
    assert_eq!(rows[1]["groups"][0]["count"].as_u64().unwrap(), 2);
}

// ── W3 C-c: `kb queries list|save|rm` — daemon-wide saved-query store ─────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queries_list_starts_empty() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["queries", "list", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("expected JSON; got:\n{text}\nerror: {e}"));
    assert_eq!(rows.as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queries_save_then_list_then_rm_round_trips() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");

    // save — defaults path to "/".
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "queries",
            "save",
            "my scene",
            "--search",
            "?view=canvas&kb=smoke",
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
    let rows: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("expected JSON; got:\n{text}\nerror: {e}"));
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"].as_str().unwrap(), "my scene");
    assert_eq!(rows[0]["path"].as_str().unwrap(), "/");
    assert_eq!(rows[0]["search"].as_str().unwrap(), "?view=canvas&kb=smoke");

    // list — sees the saved row.
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["queries", "list", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"].as_str().unwrap(), "my scene");

    // rm — removes it; a second rm is idempotent (still succeeds).
    Command::cargo_bin("kb")
        .unwrap()
        .args(["queries", "rm", "my scene", "--daemon", &url])
        .assert()
        .success();
    Command::cargo_bin("kb")
        .unwrap()
        .args(["queries", "rm", "my scene", "--daemon", &url])
        .assert()
        .success();

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["queries", "list", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queries_save_upserts_case_insensitively() {
    let (_tmp, addr) = boot().await;
    let url = format!("http://{addr}");

    for search in ["?a=1", "?a=2"] {
        Command::cargo_bin("kb")
            .unwrap()
            .args([
                "queries", "save", "Scene", "--search", search, "--daemon", &url,
            ])
            .assert()
            .success();
    }
    // Re-saved under a different case — still one row, latest value wins.
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "queries", "save", "SCENE", "--search", "?a=3", "--daemon", &url,
        ])
        .assert()
        .success();

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["queries", "list", "--daemon", &url, "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&text).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "case-insensitive name must de-dupe: {rows:?}"
    );
    assert_eq!(rows[0]["search"].as_str().unwrap(), "?a=3");
}
