//! `kb comments` integration tests. R6 made every verb HTTP-only, so the
//! substantive list/export/round-trip tests boot a real daemon (mirroring
//! `search_json.rs`); the rest are flag/help + clean-failure-on-unreachable
//! checks that don't need one.
use crate::common::canon_dir;

use assert_cmd::Command;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::time::Duration;

/// Boot a real daemon on a random port indexing the canon corpus under a
/// per-test tempdir. Duplicated from `search_json.rs` — Cargo doesn't
/// share integration-test fixtures across files.
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
        "cli-comments-{}",
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
    let url = format!("http://{addr}");
    wait_indexed(&url, &[("smoke", 4)]).await;
    (tmp, url)
}

/// Wait until the daemon's cold-boot index has settled: poll the docs list
/// until every kb reports its expected doc count. The old fixed 800 ms
/// post-boot sleep raced the CLI verbs' 5 s HTTP timeout on the
/// CPU-throttled CI container — `comments add` died with "operation timed
/// out" while the daemon was still churning through the initial index
/// (the comments.rs:93 flake, CI run 29091965575).
async fn wait_indexed(url: &str, expect: &[(&str, usize)]) {
    let client = reqwest::Client::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    for (kb, want) in expect {
        loop {
            let count = async {
                let v: serde_json::Value = client
                    .get(format!("{url}/api/kb/{kb}/docs"))
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await
                    .ok()?
                    .json()
                    .await
                    .ok()?;
                Some(v.as_array().map_or(0, |a| a.len()))
            }
            .await
            .unwrap_or(0);
            if count >= *want {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "daemon at {url} never finished indexing kb {kb} (want {want} docs, saw {count})"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

/// Run `kb comments …` against `url`, return stdout. Asserts success.
fn run_ok(url: &str, args: &[&str]) -> String {
    let mut full = vec!["comments"];
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
async fn list_and_export_round_trip_against_daemon() {
    let (_tmp, url) = boot().await;

    // Add a comment via the CLI (HTTP). Capture the assigned id.
    let added = run_ok(
        &url,
        &["add", "smoke", "rt1234567890", "--body", "what about Pin?"],
    );
    assert!(added.contains("✓ added c_"), "got: {added}");
    let cid = added
        .split_whitespace()
        .find(|t| t.starts_with("c_"))
        .expect("a c_ id in add output")
        .to_string();

    // list (default open) shows it.
    let listed = run_ok(&url, &["list", "--kb", "smoke"]);
    assert!(listed.contains("rt1234567890"), "got: {listed}");
    assert!(listed.contains("what about Pin?"), "got: {listed}");

    // --json emits a valid array (the contract the old json_output test held).
    let json = run_ok(&url, &["list", "--kb", "smoke", "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&json).expect("list --json must be valid JSON");
    assert!(parsed.is_array(), "list --json emits an array: {json}");

    // export renders the Claude prompt with the open body.
    let exported = run_ok(&url, &["export", "smoke", "rt1234567890"]);
    assert!(
        exported.contains("Open comments to address:"),
        "got: {exported}"
    );
    assert!(exported.contains("what about Pin?"));

    // resolve hides it from the default list; --all brings it back.
    run_ok(&url, &["resolve", "smoke", "rt1234567890", &cid]);
    let open_only = run_ok(&url, &["list", "--kb", "smoke"]);
    assert!(
        !open_only.contains("rt1234567890"),
        "resolved comment hidden by default"
    );
    let all = run_ok(&url, &["list", "--kb", "smoke", "--all"]);
    assert!(all.contains("rt1234567890"), "‑‑all includes resolved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reply_unresolve_edit_delete_round_trip() {
    let (_tmp, url) = boot().await;
    let added = run_ok(
        &url,
        &[
            "add",
            "smoke",
            "abcabcabc123",
            "--body",
            "orig",
            "--author",
            "you",
        ],
    );
    let cid = added
        .split_whitespace()
        .find(|t| t.starts_with("c_"))
        .unwrap()
        .to_string();

    // reply, then edit the comment body, then resolve+unresolve.
    run_ok(
        &url,
        &[
            "reply",
            &cid,
            "--kb",
            "smoke",
            "--artifact-id",
            "abcabcabc123",
            "--body",
            "ack",
        ],
    );
    run_ok(
        &url,
        &[
            "edit",
            &cid,
            "--kb",
            "smoke",
            "--artifact-id",
            "abcabcabc123",
            "--body",
            "edited body",
        ],
    );
    run_ok(&url, &["resolve", "smoke", "abcabcabc123", &cid]);
    run_ok(&url, &["unresolve", "smoke", "abcabcabc123", &cid]);

    // show reflects the edit + the reply.
    let shown = run_ok(&url, &["show", "smoke", "abcabcabc123"]);
    assert!(shown.contains("edited body"), "got: {shown}");
    assert!(shown.contains("(edited)"), "edit marker present: {shown}");
    assert!(shown.contains("ack"), "reply present: {shown}");

    // delete requires --yes; deleting clears the thread.
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "delete",
            &cid,
            "--kb",
            "smoke",
            "--artifact-id",
            "abcabcabc123",
            "--daemon",
            &url,
        ])
        .assert()
        .failure(); // no --yes
    run_ok(
        &url,
        &[
            "delete",
            &cid,
            "--kb",
            "smoke",
            "--artifact-id",
            "abcabcabc123",
            "--yes",
        ],
    );
    let after = run_ok(&url, &["show", "smoke", "abcabcabc123"]);
    assert!(after.contains("(no comments)"), "thread emptied: {after}");
}

// --- clean failure when the daemon is unreachable --------------------------

#[test]
fn list_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "list",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn export_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "export",
            "smoke",
            "abc123def456",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

// --- help / flag surface (no daemon needed) --------------------------------

#[test]
fn comments_list_help_lists_filters() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "list", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--author"));
    assert!(text.contains("--stale"));
    assert!(text.contains("--folder"));
}

#[test]
fn comments_show_help_lists_path_and_daemon() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "show", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--path"));
    assert!(text.contains("--daemon"));
}

#[test]
fn comments_export_help_lists_format() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "export", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--format"));
}

#[test]
fn comments_unresolve_help_lists_all_flag() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "unresolve", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--all"));
    assert!(text.contains("--daemon"));
}

#[test]
fn comments_edit_help_lists_reply_and_body() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "edit", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--reply"));
    assert!(text.contains("--body"));
}

#[test]
fn comments_delete_help_lists_yes_guard() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "delete", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--yes"));
    assert!(text.contains("--reply"));
}

#[test]
fn comments_resolve_help_lists_required_positionals_and_daemon_flag() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "resolve", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--daemon"), "missing --daemon flag in help");
}

#[test]
fn comments_resolve_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "resolve",
            "smoke",
            "abc123def456",
            "c_1",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn comments_reply_help_lists_body_and_daemon_flag() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "reply", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--body"), "missing --body flag in help");
    assert!(text.contains("--path"), "missing --path flag in help");
    assert!(text.contains("--daemon"), "missing --daemon flag in help");
}

#[test]
fn comments_reply_requires_comment_id_positional() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "reply", "--path", "x.html", "--body", "hi"])
        .assert()
        .failure();
}

#[test]
fn comments_reply_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "reply",
            "c_1",
            "--artifact-id",
            "abc123def456",
            "--kb",
            "smoke",
            "--body",
            "looks good",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn comments_watch_help_lists_flags() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "watch", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--path"));
    assert!(text.contains("--json"));
    assert!(text.contains("--once"));
    assert!(text.contains("--timeout"));
    assert!(text.contains("--backlog"));
}

#[test]
fn comments_watch_requires_path() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "watch", "--once"])
        .assert()
        .failure();
}

#[test]
fn comments_watch_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "watch",
            "--path",
            "x.html",
            "--once",
            "--timeout",
            "1",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn comments_add_help_lists_body_anchor_author() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "add", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--body"));
    assert!(text.contains("--anchor"));
    assert!(text.contains("--author"));
    assert!(text.contains("--page"));
    assert!(text.contains("--daemon"));
}

#[test]
fn comments_add_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "add",
            "smoke",
            "abc123",
            "--body",
            "test",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn comments_add_rejects_unknown_anchor() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "add",
            "smoke",
            "abc123",
            "--body",
            "test",
            "--anchor",
            "made-up-kind",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

// --- reanchor (R9) ---------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reanchor_round_trip_against_daemon() {
    let (_tmp, url) = boot().await;
    // Add a whole-file comment, then re-point it to a section anchor.
    let added = run_ok(
        &url,
        &[
            "add",
            "smoke",
            "anch12345678",
            "--body",
            "moved?",
            "--author",
            "you",
        ],
    );
    let cid = added
        .split_whitespace()
        .find(|t| t.starts_with("c_"))
        .unwrap()
        .to_string();

    let out = run_ok(
        &url,
        &[
            "reanchor",
            &cid,
            "--kb",
            "smoke",
            "--artifact-id",
            "anch12345678",
            "--anchor",
            "section:overview",
        ],
    );
    assert!(out.contains("✓ reanchored"), "got: {out}");

    // show reflects the new section anchor.
    let shown = run_ok(&url, &["show", "smoke", "anch12345678"]);
    assert!(
        shown.contains("section:overview"),
        "anchor re-pointed: {shown}"
    );
}

#[test]
fn comments_reanchor_help_lists_anchor() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "reanchor", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--anchor"));
    assert!(text.contains("--daemon"));
}

#[test]
fn comments_reanchor_requires_comment_id_positional() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "reanchor",
            "--anchor",
            "section:x",
            "--artifact-id",
            "abc123def456",
            "--kb",
            "smoke",
        ])
        .assert()
        .failure();
}

#[test]
fn comments_reanchor_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "reanchor",
            "c_1",
            "--artifact-id",
            "abc123def456",
            "--kb",
            "smoke",
            "--anchor",
            "section:overview",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn comments_reanchor_rejects_unknown_anchor() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "reanchor",
            "c_1",
            "--artifact-id",
            "abc123def456",
            "--kb",
            "smoke",
            "--anchor",
            "made-up-kind",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

// --- flag normalization + kb auto-resolve + anchor_label -------------------

/// Run `kb comments …`, assert FAILURE, return stderr (for the auto-resolve
/// ambiguous/not-found error-message assertions).
fn run_fail(url: &str, args: &[&str]) -> String {
    let mut full = vec!["comments"];
    full.extend_from_slice(args);
    full.extend_from_slice(&["--daemon", url]);
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(&full)
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    String::from_utf8(out).unwrap()
}

/// Boot a real daemon with TWO kbs ("smoke" + "smoke2"), each indexing a
/// copy of the canon corpus under its own tempdir subtree. Used by the kb
/// auto-resolve tests (the behaviour only kicks in with >1 kb configured).
async fn boot2() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    for name in ["smoke", "smoke2"] {
        let source = tmp.path().join(name);
        std::fs::create_dir_all(&source).unwrap();
        for f in ["kitchen-sink.html", "cost-of-abstraction.html"] {
            std::fs::copy(canon_dir().join(f), source.join(f))
                .unwrap_or_else(|e| panic!("copy {f}: {e}"));
        }
        kb_map.insert(
            KbName::new(name).unwrap(),
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
    }
    let daemon_name = format!(
        "cli-comments2-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
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
    let url = format!("http://{addr}");
    wait_indexed(&url, &[("smoke", 2), ("smoke2", 2)]).await;
    (tmp, url)
}

#[test]
fn positional_verbs_help_lists_kb_and_artifact_id_flags() {
    for verb in ["show", "export", "add", "resolve", "unresolve"] {
        let out = Command::cargo_bin("kb")
            .unwrap()
            .args(["comments", verb, "--help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("--kb"), "{verb} --help missing --kb:\n{text}");
        assert!(
            text.contains("--artifact-id"),
            "{verb} --help missing --artifact-id:\n{text}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flag_forms_and_shift_on_positional_verbs() {
    let (_tmp, url) = boot().await;
    // Add via the new flag form (--kb/--artifact-id) instead of positionals.
    run_ok(
        &url,
        &[
            "add",
            "--kb",
            "smoke",
            "--artifact-id",
            "ff1234567890",
            "--body",
            "flagged",
        ],
    );
    // show/export via the full flag form.
    let shown = run_ok(
        &url,
        &["show", "--kb", "smoke", "--artifact-id", "ff1234567890"],
    );
    assert!(shown.contains("flagged"), "flag-form show: {shown}");
    let exported = run_ok(
        &url,
        &["export", "--kb", "smoke", "--artifact-id", "ff1234567890"],
    );
    assert!(exported.contains("flagged"), "flag-form export: {exported}");
    // The headline complaint: `--kb <name> <bare-id>`. clap parks the bare id
    // in the kb positional slot; the reconciler shifts it to artifact_id.
    let shown2 = run_ok(&url, &["show", "--kb", "smoke", "ff1234567890"]);
    assert!(
        shown2.contains("flagged"),
        "--kb + bare id (show): {shown2}"
    );
    let exported2 = run_ok(&url, &["export", "--kb", "smoke", "ff1234567890"]);
    assert!(
        exported2.contains("flagged"),
        "--kb + bare id (export): {exported2}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_resolves_kb_from_artifact_id_unique_owner() {
    let (_tmp, url) = boot2().await;
    // A comment exists only in smoke2.
    run_ok(
        &url,
        &[
            "add",
            "--kb",
            "smoke2",
            "--artifact-id",
            "uniq00000001",
            "--body",
            "only here",
        ],
    );
    // show WITHOUT --kb auto-resolves to smoke2 via the review-space scan.
    let shown = run_ok(&url, &["show", "--artifact-id", "uniq00000001"]);
    assert!(
        shown.contains("only here"),
        "auto-resolved to smoke2: {shown}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_resolve_ambiguous_artifact_id_errors() {
    let (_tmp, url) = boot2().await;
    run_ok(
        &url,
        &[
            "add",
            "--kb",
            "smoke",
            "--artifact-id",
            "dup000000001",
            "--body",
            "in smoke",
        ],
    );
    run_ok(
        &url,
        &[
            "add",
            "--kb",
            "smoke2",
            "--artifact-id",
            "dup000000001",
            "--body",
            "in smoke2",
        ],
    );
    let err = run_fail(&url, &["show", "--artifact-id", "dup000000001"]);
    assert!(err.contains("multiple kbs"), "ambiguous error: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_resolve_unknown_artifact_id_errors() {
    let (_tmp, url) = boot2().await;
    let err = run_fail(&url, &["show", "--artifact-id", "deadbeef0000"]);
    assert!(err.contains("not found in any"), "not-found error: {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_json_carries_anchor_label() {
    let (_tmp, url) = boot().await;
    run_ok(
        &url,
        &[
            "add",
            "--kb",
            "smoke",
            "--artifact-id",
            "al1234567890",
            "--body",
            "sectioned",
            "--anchor",
            "section:overview",
        ],
    );
    run_ok(
        &url,
        &[
            "add",
            "--kb",
            "smoke",
            "--artifact-id",
            "al1234567890",
            "--body",
            "whole file", // default --anchor file
        ],
    );
    let json = run_ok(&url, &["list", "--kb", "smoke", "--all", "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let labels: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["anchor_label"].as_str())
        .collect();
    assert!(
        labels.contains(&"section:overview"),
        "section anchor_label present: {json}"
    );
    assert!(
        labels.contains(&"file"),
        "file anchor_label present: {json}"
    );
}
