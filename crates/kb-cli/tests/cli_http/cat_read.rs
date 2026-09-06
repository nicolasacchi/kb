//! `kb cat` / `kb read` integration tests — GC-B5 (roadmap G17).
//!
//! Pins two fixes documented in
//! `docs/research/kb-cat-read-daemon-resolution-defect-2026-07.html`:
//!  1. both verbs now try the daemon first (mirroring `search`/`download`),
//!     falling back to a local read-only lance open only when unreachable;
//!  2. a daemon-backed read records a `history` "open" row tagged
//!     `source: "cli"` by default (`--no-record` opts out; a local-fs-only
//!     read needs explicit `--record`, which errors cleanly with no
//!     reachable daemon).
//!
//! Boot pattern mirrors `download.rs`/`comments.rs` (real daemon, tempdir
//! corpus, random port).
use crate::common;
use crate::common::{base_config, canon_dir, kb_section};

use assert_cmd::Command;
use kb_core::config::KbSection;
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Boot a daemon indexing a tempdir corpus: the four flat canon files plus
/// a `notes/` subfolder with two HTML docs (exercises the subfolder
/// permalink path for `kb read`). Returns `(tempdir, daemon_name, source,
/// base_url)`.
async fn boot() -> (tempfile::TempDir, String, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(source.join("notes")).unwrap();
    for name in [
        "fullscreen-viz.html",
        "kitchen-sink.html",
        "multi-page.html",
        "cost-of-abstraction.html",
    ] {
        std::fs::copy(canon_dir().join(name), source.join(name))
            .unwrap_or_else(|e| panic!("copy {name}: {e}"));
    }
    for n in ["one", "two"] {
        std::fs::write(
            source.join("notes").join(format!("{n}.html")),
            format!("<!doctype html><html><head><title>note {n}</title></head><body><h1>note {n}</h1></body></html>"),
        )
        .unwrap();
    }

    let daemon_name = format!(
        "cli-catread-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source.clone()));
    let cfg = base_config(&daemon_name, kb_map);
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name.clone());
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(900)).await;
    (tmp, daemon_name, source, format!("http://{addr}"))
}

/// Write a local `kb.toml` (single kb `smoke` → `source`) rooted so that
/// `KB_HOME=<root>` makes `KbPaths::new(daemon_name)` resolve to the exact
/// same state dir the daemon in `boot()` wrote to
/// (`KbPaths::rooted_at(root, daemon_name)` and `KbPaths::new` under
/// `KB_HOME` compose identically). Returns the config file path.
fn write_local_config(root: &Path, daemon_name: &str, source: &Path) -> PathBuf {
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        kb_section(source.to_path_buf()),
    );
    let cfg = base_config(daemon_name, kb_map);
    let path = root.join("local-kb.toml");
    cfg.save(&path).expect("write local kb.toml");
    path
}

/// MI test-hardening (2026-08) — `boot()`'s fixed post-boot sleep is a
/// head-start, not a guarantee (see `common`'s module doc): under host I/O
/// contention the indexer can still be mid-walk when this first runs, in
/// which case `kb find` legitimately finds nothing yet. Retry with a
/// load-aware deadline instead of a single-shot assert that turns a timing
/// race into a false-red failure.
fn find_id(url: &str, target: &str) -> String {
    common::poll_until_sync(
        &format!("`kb find {target}` to resolve via the daemon"),
        || {
            let output = Command::cargo_bin("kb")
                .unwrap()
                .args(["find", target, "--kb", "smoke", "--daemon", url])
                .output()
                .expect("spawn kb find");
            if !output.status.success() {
                return None;
            }
            let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (id.len() == 12).then_some(id)
        },
    )
}

/// `GET /api/kb/smoke/history?kind=open` → the `entries` array.
async fn open_history_rows(url: &str) -> Vec<serde_json::Value> {
    let resp = reqwest::Client::new()
        .get(format!("{url}/api/kb/smoke/history?kind=open&limit=50"))
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    body["entries"].as_array().cloned().unwrap_or_default()
}

// --- daemon-path: recording default-on / --no-record ------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_via_daemon_streams_bytes_and_records_one_history_row() {
    let (_tmp, _name, _source, url) = boot().await;
    let id = find_id(&url, "fullscreen-viz.html");

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["cat", &id, "--kb", "smoke", "--daemon", &url])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&out).starts_with('<'),
        "raw HTML on stdout"
    );

    let rows = open_history_rows(&url).await;
    let mine: Vec<_> = rows
        .iter()
        .filter(|r| r["artifact_id"] == id.as_str())
        .collect();
    assert_eq!(mine.len(), 1, "exactly one history row for {id}: {rows:?}");
    assert_eq!(mine[0]["open_source"].as_str(), Some("cli"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_no_record_skips_history() {
    let (_tmp, _name, _source, url) = boot().await;
    let id = find_id(&url, "kitchen-sink.html");

    Command::cargo_bin("kb")
        .unwrap()
        .args(["cat", &id, "--kb", "smoke", "--daemon", &url, "--no-record"])
        .assert()
        .success();

    let rows = open_history_rows(&url).await;
    assert!(
        rows.iter().all(|r| r["artifact_id"] != id.as_str()),
        "no history row expected: {rows:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_not_found_via_daemon_names_the_daemon() {
    let (_tmp, _name, _source, url) = boot().await;
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["cat", "deadbeef0000", "--kb", "smoke", "--daemon", &url])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("via daemon"),
        "not-found error should name the daemon store: {text}"
    );
}

// --- offline / daemon-unreachable path: no recording by default -------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_offline_flag_serves_local_bytes_and_records_nothing() {
    // Acceptance criterion 2 from the defect doc: with a local single-kb
    // kb.toml, `--offline` still works exactly as it did before this fix
    // (regression guard), independent of any daemon's reachability — an
    // explicit `--daemon` that turns out unreachable is a hard error (it
    // "forces HTTP", mirroring `search`), so this is tested via the
    // deterministic `--offline` flag rather than by racing a dead port.
    let (tmp, daemon_name, source, url) = boot().await;
    let id = find_id(&url, "multi-page.html");
    let cfg_path = write_local_config(tmp.path(), &daemon_name, &source);

    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args([
            "cat",
            &id,
            "--kb",
            "smoke",
            "--offline",
            "--config",
            cfg_path.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(
        String::from_utf8_lossy(&out).starts_with('<'),
        "raw HTML from the local lance offline path"
    );

    let rows = open_history_rows(&url).await;
    assert!(
        rows.iter().all(|r| r["artifact_id"] != id.as_str()),
        "offline fallback must not record by default: {rows:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_offline_not_found_names_local_lance_state() {
    let (tmp, daemon_name, source, _url) = boot().await;
    let cfg_path = write_local_config(tmp.path(), &daemon_name, &source);

    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args([
            "cat",
            "deadbeef0000",
            "--kb",
            "smoke",
            "--offline",
            "--config",
            cfg_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("LOCAL lance state") && text.contains("daemon wasn't consulted"),
        "offline not-found error should name the local store: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_record_flag_errors_cleanly_with_no_reachable_daemon() {
    let (tmp, daemon_name, source, url) = boot().await;
    let cfg_path = write_local_config(tmp.path(), &daemon_name, &source);
    let id = find_id(&url, "multi-page.html");

    // Unlike the other tests in this file, this one's whole point is to
    // exercise "--record with no reachable daemon" — it can't sidestep the
    // question of reachability the way `--offline`-only tests do. Omitting
    // `--daemon` here would let `--record` fall back to detect_daemon's own
    // default (127.0.0.1:4000), which races whatever happens to already be
    // bound there on the machine running the test (e.g. a real, long-lived
    // `kb daemon` used as local agent memory) — an explicit, near-certainly-
    // closed port makes "unreachable" deterministic instead.
    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .args([
            "cat",
            &id,
            "--kb",
            "smoke",
            "--offline",
            "--record",
            "--daemon",
            "http://127.0.0.1:1",
            "--config",
            cfg_path.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("--record") && text.contains("reachable daemon"),
        "expected a polite --record-needs-a-daemon error: {text}"
    );
}

// --- zero local kbs vs many local kbs: distinct, non-misleading errors -

#[tokio::test]
async fn cat_zero_local_kbs_and_unreachable_daemon_is_self_diagnosing() {
    // Acceptance criterion 3 from the defect doc, exercised deterministically
    // via `--offline` (no local kb.toml at all) rather than by racing a dead
    // default-daemon port.
    let tmp = tempfile::tempdir().unwrap();
    let missing_cfg = tmp.path().join("does-not-exist.toml");

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "cat",
            "deadbeef0000",
            "--offline",
            "--config",
            missing_cfg.to_str().unwrap(),
        ])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("no kb configured"),
        "zero local kbs should say so plainly: {text}"
    );
    assert!(
        !text.contains("multiple kbs are configured"),
        "must not claim multiple kbs are configured when there are zero: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cat_multi_kb_daemon_reports_daemon_side_count_not_local() {
    // Two kbs on the daemon, zero local kbs configured — before the fix
    // this always printed the LOCAL "must specify --kb" message even
    // though the daemon (not the local config) is what actually needs
    // disambiguating. After the fix the daemon path surfaces its own
    // (accurate) kb count.
    let tmp = tempfile::tempdir().unwrap();
    let source_a = tmp.path().join("a");
    let source_b = tmp.path().join("b");
    std::fs::create_dir_all(&source_a).unwrap();
    std::fs::create_dir_all(&source_b).unwrap();
    std::fs::copy(
        canon_dir().join("fullscreen-viz.html"),
        source_a.join("fullscreen-viz.html"),
    )
    .unwrap();
    std::fs::copy(
        canon_dir().join("kitchen-sink.html"),
        source_b.join("kitchen-sink.html"),
    )
    .unwrap();
    let daemon_name = format!(
        "cli-catread-multi-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("a").unwrap(), kb_section(source_a));
    kb_map.insert(KbName::new("b").unwrap(), kb_section(source_b));
    let cfg = base_config(&daemon_name, kb_map);
    let paths = KbPaths::rooted_at(tmp.path(), daemon_name.clone());
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(900)).await;
    let url = format!("http://{addr}");

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["cat", "deadbeef0000", "--daemon", &url])
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    let text = String::from_utf8_lossy(&out);
    assert!(
        text.contains("2 kbs configured"),
        "expected the daemon's own kb count, got: {text}"
    );
}

// === kb read ============================================================
//
// `read` spawns a browser opener; `KB_OPENER` (test-only seam, see
// `commands::read`) points it at a tiny recorder script instead so these
// tests can assert on the exact URL/path `read` would have opened without
// touching a real browser.

/// Write an executable recorder script that appends its first argument to
/// `record` (path taken from `$KB_TEST_RECORD` so the child inherits it).
/// Returns the script path.
fn opener_script(tmp: &Path, record: &Path) -> PathBuf {
    let script = tmp.join("fake-opener.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > \"{}\"\n", record.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&script).unwrap().permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&script, perm).unwrap();
    }
    script
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_via_daemon_opens_spa_permalink_and_records() {
    let (tmp, _name, _source, url) = boot().await;
    let id = find_id(&url, "fullscreen-viz.html");
    let record = tmp.path().join("opened.txt");
    let script = opener_script(tmp.path(), &record);

    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_OPENER", &script)
        .args(["read", &id, "--kb", "smoke", "--daemon", &url])
        .assert()
        .success();

    let opened = std::fs::read_to_string(&record).expect("opener recorded a target");
    assert_eq!(opened, format!("{url}/a/smoke/fullscreen-viz.html"));

    let rows = open_history_rows(&url).await;
    let mine: Vec<_> = rows
        .iter()
        .filter(|r| r["artifact_id"] == id.as_str())
        .collect();
    assert_eq!(mine.len(), 1, "exactly one history row for {id}: {rows:?}");
    assert_eq!(mine[0]["open_source"].as_str(), Some("cli"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_via_daemon_subfolder_permalink_encodes_each_segment() {
    let (tmp, _name, _source, url) = boot().await;
    let id = find_id(&url, "notes/one.html");
    let record = tmp.path().join("opened.txt");
    let script = opener_script(tmp.path(), &record);

    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_OPENER", &script)
        .args(["read", &id, "--kb", "smoke", "--daemon", &url])
        .assert()
        .success();

    let opened = std::fs::read_to_string(&record).expect("opener recorded a target");
    assert_eq!(opened, format!("{url}/a/smoke/notes/one.html"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_no_record_skips_history() {
    let (tmp, _name, _source, url) = boot().await;
    let id = find_id(&url, "kitchen-sink.html");
    let record = tmp.path().join("opened.txt");
    let script = opener_script(tmp.path(), &record);

    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_OPENER", &script)
        .args([
            "read",
            &id,
            "--kb",
            "smoke",
            "--daemon",
            &url,
            "--no-record",
        ])
        .assert()
        .success();

    let rows = open_history_rows(&url).await;
    assert!(
        rows.iter().all(|r| r["artifact_id"] != id.as_str()),
        "no history row expected: {rows:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_offline_flag_opens_local_path_and_records_nothing() {
    // Same rationale as `cat_offline_flag_serves_local_bytes_and_records_nothing`
    // — `--offline` is the deterministic way to exercise the local-lance
    // path regardless of any daemon's reachability.
    let (tmp, daemon_name, source, url) = boot().await;
    let id = find_id(&url, "multi-page.html");
    let cfg_path = write_local_config(tmp.path(), &daemon_name, &source);
    let record = tmp.path().join("opened.txt");
    let script = opener_script(tmp.path(), &record);

    Command::cargo_bin("kb")
        .unwrap()
        .env("KB_HOME", tmp.path())
        .env("KB_OPENER", &script)
        .args([
            "read",
            &id,
            "--kb",
            "smoke",
            "--offline",
            "--config",
            cfg_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // The offline path opens the local file path, not a permalink URL.
    let opened = std::fs::read_to_string(&record).expect("opener recorded a target");
    assert!(
        opened.ends_with("multi-page.html") && !opened.starts_with("http"),
        "expected a local file path, got {opened:?}"
    );

    let rows = open_history_rows(&url).await;
    assert!(
        rows.iter().all(|r| r["artifact_id"] != id.as_str()),
        "offline fallback must not record by default: {rows:?}"
    );
}

// === kb get =============================================================
//
// GC-F1: `get` is purely daemon-backed (no offline path), so a successful
// fetch records the same `source: "cli"` history "open" row as cat/read,
// default on, with `--no-record` as the opt-out.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_via_daemon_prints_metadata_and_records_one_history_row() {
    let (_tmp, _name, _source, url) = boot().await;
    let id = find_id(&url, "fullscreen-viz.html");

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["get", &id, "--kb", "smoke", "--daemon", &url])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let body: serde_json::Value =
        serde_json::from_slice(&out).expect("kb get default format is JSON metadata");
    assert_eq!(body["id"].as_str(), Some(id.as_str()));

    let rows = open_history_rows(&url).await;
    let mine: Vec<_> = rows
        .iter()
        .filter(|r| r["artifact_id"] == id.as_str())
        .collect();
    assert_eq!(mine.len(), 1, "exactly one history row for {id}: {rows:?}");
    assert_eq!(mine[0]["open_source"].as_str(), Some("cli"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_no_record_skips_history() {
    let (_tmp, _name, _source, url) = boot().await;
    let id = find_id(&url, "kitchen-sink.html");

    Command::cargo_bin("kb")
        .unwrap()
        .args(["get", &id, "--kb", "smoke", "--daemon", &url, "--no-record"])
        .assert()
        .success();

    let rows = open_history_rows(&url).await;
    assert!(
        rows.iter().all(|r| r["artifact_id"] != id.as_str()),
        "no history row expected: {rows:?}"
    );
}
