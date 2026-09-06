//! `kb download` integration tests. The download verb is HTTP-only, so
//! the substantive tests boot a real daemon (mirroring `comments.rs` /
//! `search_json.rs`); the rest are help/clean-failure checks that don't
//! need one.
use crate::common::canon_dir;

use assert_cmd::Command;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

/// Boot a daemon indexing a tempdir corpus: the four flat canon files plus
/// a `notes/` subfolder with two HTML docs (so the folder-zip path has a
/// real descendant set). Returns the tempdir + the daemon base URL.
async fn boot() -> (tempfile::TempDir, String) {
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
        "cli-download-{}",
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

/// Run `kb download …` against `url`, asserting success; returns raw stdout.
fn download_ok(url: &str, args: &[&str]) -> Vec<u8> {
    let mut full = vec!["download"];
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
    out
}

fn zip_entry_names(bytes: &[u8]) -> Vec<String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("valid zip");
    let mut names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_artifact_by_path_to_stdout_and_file() {
    let (tmp, url) = boot().await;

    // By source-relative path (resolved via /lookup) → stdout: raw HTML.
    let stdout = download_ok(&url, &["fullscreen-viz.html", "--kb", "smoke"]);
    let text = String::from_utf8_lossy(&stdout);
    assert!(
        text.contains("<"),
        "expected HTML on stdout, got: {text:.120}"
    );

    // -o writes the same bytes to a file (status line goes to stderr).
    let out = tmp.path().join("a.html");
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "download",
            "fullscreen-viz.html",
            "--kb",
            "smoke",
            "-o",
            out.to_str().unwrap(),
            "--daemon",
            &url,
        ])
        .assert()
        .success();
    let body = std::fs::read(&out).expect("output file written");
    assert!(!body.is_empty(), "file should carry the artifact bytes");
    assert!(body.starts_with(b"<"), "raw HTML source");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn single_artifact_by_hex_id() {
    let (_tmp, url) = boot().await;
    // `kb find` resolves a filename to its 12-hex id; feed that straight
    // to `kb download` to exercise the direct-id (no-lookup) branch.
    let id_out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "find",
            "kitchen-sink.html",
            "--kb",
            "smoke",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let id = String::from_utf8_lossy(&id_out).trim().to_string();
    assert_eq!(id.len(), 12, "kb find prints a 12-hex id, got {id:?}");

    let stdout = download_ok(&url, &[&id, "--kb", "smoke"]);
    assert!(stdout.starts_with(b"<"), "raw HTML for id {id}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn folder_zip_to_file_contains_descendants() {
    let (tmp, url) = boot().await;
    let out = tmp.path().join("notes.zip");
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "download",
            "--folder",
            "notes",
            "--kb",
            "smoke",
            "-o",
            out.to_str().unwrap(),
            "--daemon",
            &url,
        ])
        .assert()
        .success();
    let bytes = std::fs::read(&out).unwrap();
    let names = zip_entry_names(&bytes);
    assert_eq!(
        names,
        vec!["notes/one.html".to_string(), "notes/two.html".to_string()],
        "got {names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whole_kb_zip_to_stdout_is_binary_safe() {
    let (_tmp, url) = boot().await;
    // assert_cmd captures stdout (not a TTY), so the zip-to-terminal guard
    // stays off and the raw archive streams to stdout.
    let bytes = download_ok(&url, &["--all", "--kb", "smoke"]);
    assert!(bytes.starts_with(b"PK"), "zip magic bytes on stdout");
    let names = zip_entry_names(&bytes);
    // 4 flat canon docs + 2 notes/ docs = 6.
    assert_eq!(names.len(), 6, "got {names:?}");
    assert!(
        names.contains(&"notes/one.html".to_string()),
        "got {names:?}"
    );
    assert!(
        names.contains(&"fullscreen-viz.html".to_string()),
        "got {names:?}"
    );
    // Round-trips through the zip reader end-to-end.
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut buf = String::new();
    zip.by_name("notes/one.html")
        .unwrap()
        .read_to_string(&mut buf)
        .unwrap();
    assert!(buf.contains("note one"), "entry body intact: {buf:.80}");
}

// ---------- lightweight (no daemon) ----------

#[test]
fn download_help_lists_flags() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["download", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8_lossy(&out);
    for needle in ["--folder", "--all", "--out", "--kb", "--daemon"] {
        assert!(text.contains(needle), "help missing {needle}: {text}");
    }
}

#[test]
fn download_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "download",
            "deadbeef0000",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn download_with_no_target_or_folder_errors() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "download",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}
