//! `kb notes` integration tests. Every verb is HTTP-only (it drives the
//! daemon), so this boots a real daemon (mirroring `comments.rs`) and round-
//! trips new → list → show → check → append → done → rm. The `kb notes`
//! surface previously had ZERO coverage.
use crate::common;
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
/// tempdir. Mirrors `comments.rs::boot` (Cargo doesn't share fixtures across
/// integration-test files).
async fn boot() -> (tempfile::TempDir, String) {
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
        "cli-notes-{}",
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

/// Run `kb notes …` against `url`, asserting success; returns stdout.
///
/// Passes `KB_TEST_HTTP_TIMEOUT_SECS` through to the spawned `kb` process
/// (MI test-hardening, 2026-08 — see `common::http_timeout_secs`'s doc) so a
/// mutating call like `new` tolerates a daemon that's merely slow under host
/// I/O contention rather than tripping its own call-site-hardcoded timeout.
fn run_ok(url: &str, args: &[&str]) -> String {
    let mut full = vec!["notes"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["--daemon", url]);
    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("KB_TEST_HTTP_TIMEOUT_SECS", common::http_timeout_secs())
        .args(&full)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

/// Poll the daemon until note `id` is indexed (appears in the per-kb list),
/// against a load-aware deadline (`common::index_wait_deadline`) rather than
/// a hardcoded 10s — MI test-hardening (2026-08), see `common`'s module doc.
async fn wait_indexed(client: &reqwest::Client, url: &str, id: &str) {
    common::poll_until(&format!("note {id} to be indexed"), || async {
        let v: serde_json::Value = client
            .get(format!("{url}/api/kb/smoke/notes"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        let found = v["notes"]
            .as_array()
            .map(|a| a.iter().any(|n| n["id"].as_str() == Some(id)))
            .unwrap_or(false);
        found.then_some(())
    })
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notes_cli_round_trip_against_daemon() {
    let (_tmp, url) = boot().await;
    let client = reqwest::Client::new();

    // new → "created <id>  <path>".
    let created = run_ok(
        &url,
        &[
            "new",
            "--kb",
            "smoke",
            "--title",
            "CLI checklist",
            "--body",
            "- [ ] one\n- [ ] two",
        ],
    );
    assert!(created.starts_with("created "), "got: {created}");
    let id = created
        .split_whitespace()
        .nth(1)
        .expect("an id in `created` output")
        .to_string();
    assert_eq!(id.len(), 12, "12-hex id, got: {created}");
    wait_indexed(&client, &url, &id).await;

    // list --json includes it.
    let listed = run_ok(&url, &["list", "--kb", "smoke", "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&listed).expect("list --json must be valid JSON");
    let arr = parsed
        .get("notes")
        .and_then(|n| n.as_array())
        .or_else(|| parsed.as_array())
        .expect("a notes array");
    assert!(
        arr.iter().any(|n| n["id"].as_str() == Some(&id)),
        "list --json missing the note: {listed}"
    );

    // check item 0 → 1/2 done.
    let checked = run_ok(&url, &["check", &id, "--item", "0", "--kb", "smoke"]);
    assert!(checked.contains("(1/2 done)"), "got: {checked}");

    // append → 1/3 done.
    let appended = run_ok(&url, &["append", &id, "--item", "three", "--kb", "smoke"]);
    assert!(appended.contains("(1/3 done)"), "got: {appended}");

    // show --json reflects the appended task in the body.
    let shown = run_ok(&url, &["show", &id, "--kb", "smoke", "--json"]);
    let detail: serde_json::Value =
        serde_json::from_str(&shown).expect("show --json must be valid JSON");
    assert!(
        detail["body_md"]
            .as_str()
            .unwrap_or("")
            .contains("- [ ] three"),
        "show body missing appended task: {shown}"
    );

    // done sets status, rm deletes (requires --yes).
    run_ok(&url, &["done", &id, "--kb", "smoke"]);
    run_ok(&url, &["rm", &id, "--yes", "--kb", "smoke"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notes_cli_rm_requires_yes_and_unreachable_daemon_fails_cleanly() {
    let (_tmp, url) = boot().await;

    // rm without --yes refuses (non-zero), never deletes.
    Command::cargo_bin("kb")
        .unwrap()
        .args(["notes", "rm", "someid", "--kb", "smoke", "--daemon", &url])
        .assert()
        .failure();

    // An unreachable daemon fails cleanly (non-zero, no panic).
    Command::cargo_bin("kb")
        .unwrap()
        .args(["notes", "list", "--daemon", "http://127.0.0.1:1"])
        .assert()
        .failure();
}

/// Create a note via the CLI and return its 12-hex id.
fn cli_new(url: &str, title: &str, body: &str) -> String {
    let created = run_ok(
        url,
        &["new", "--kb", "smoke", "--title", title, "--body", body],
    );
    created
        .split_whitespace()
        .nth(1)
        .expect("an id in `created` output")
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn notes_cli_links_and_backlinks() {
    let (_tmp, url) = boot().await;
    let client = reqwest::Client::new();

    // Target note B first, so the source note's wikilink resolves at index time.
    let b_id = cli_new(&url, "Target Note", "the target body");
    wait_indexed(&client, &url, &b_id).await;

    // Source note A links to B by title + one dangling target.
    let a_id = cli_new(&url, "Source Note", "see [[Target Note]] and [[Nope]]");
    wait_indexed(&client, &url, &a_id).await;

    // `kb notes links A --json` — outgoing resolves Target Note → B, Nope dangles.
    // The route resolves through the per-(kb, index-generation) `links_index`
    // memo (root invariant #15); `wait_indexed` above only confirms A/B are
    // *stored*, not that a request-time rebuild of that memo has observed
    // them, so poll here too (same shape as the backlinks poll below) instead
    // of a single-shot assert — a transient miss/ambiguous reads as "not yet
    // settled", not a real defect. Load-aware deadline (MI test-hardening,
    // 2026-08) — see `common`'s module doc.
    let links = common::poll_until(&format!("Target Note to resolve to {b_id}"), || async {
        let links = run_ok(&url, &["links", &a_id, "--kb", "smoke", "--json"]);
        let parsed: serde_json::Value =
            serde_json::from_str(&links).expect("links --json must be valid JSON");
        let outgoing = parsed["outgoing"].as_array().expect("outgoing array");
        let settled = outgoing.iter().any(|l| {
            l["target"] == "Target Note"
                && l["state"] == "resolved"
                && l["id"].as_str() == Some(b_id.as_str())
        });
        settled.then_some(links)
    })
    .await;
    let parsed: serde_json::Value =
        serde_json::from_str(&links).expect("links --json must be valid JSON");
    let outgoing = parsed["outgoing"].as_array().expect("outgoing array");
    let resolved = outgoing
        .iter()
        .find(|l| l["target"] == "Target Note")
        .expect("the resolving target");
    assert_eq!(resolved["state"], "resolved");
    assert_eq!(resolved["id"].as_str(), Some(b_id.as_str()));
    assert!(
        outgoing
            .iter()
            .any(|l| l["target"] == "Nope" && l["state"] == "dangling"),
        "the dangling target: {links}"
    );

    // Poll the backlinks API until the A→B edge is committed (the edge-record
    // hook writes just after the upsert that made A listable). Load-aware
    // deadline (MI test-hardening, 2026-08) — see `common`'s module doc.
    common::poll_until(&format!("backlink {a_id} -> {b_id} to appear"), || async {
        let v: serde_json::Value = client
            .get(format!("{url}/api/kb/smoke/backlinks/{b_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap_or_default();
        let has = v["backlinks"]
            .as_array()
            .map(|a| a.iter().any(|b| b["id"].as_str() == Some(a_id.as_str())))
            .unwrap_or(false);
        has.then_some(())
    })
    .await;

    // `kb backlinks B --json` (top-level verb) — B is referenced by A.
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "backlinks",
            &b_id,
            "--kb",
            "smoke",
            "--json",
            "--daemon",
            &url,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let bl: serde_json::Value =
        serde_json::from_str(&String::from_utf8(out).unwrap()).expect("backlinks --json");
    assert!(
        bl["backlinks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["id"].as_str() == Some(a_id.as_str())),
        "kb backlinks missing the linker: {bl}"
    );
}
