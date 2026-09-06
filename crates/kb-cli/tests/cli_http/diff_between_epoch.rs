//! MI-W2.R — `kb diff --between <D1> <D2> --json` must carry the
//! MI-W2.4c epoch-honesty verdict as explicit fields
//! (`tombstone_era_started_unix` / `epoch_caveat`), not just as a
//! human-readable stdout note. Reviewer finding: the caveat fetch was
//! wrapped in `if !json_out`, so the agent-facing `--json` mode silently
//! dropped it — see `commands::versions::diff_between`'s doc comment.
//!
//! Boots a real daemon over a tiny git-versioned corpus (two commits),
//! pre-seeds `tombstone-era.json` with a far-future `started_unix` so the
//! requested `--between` window deterministically predates it (no need to
//! actually forget/purge anything to exercise the caveat), and drives the
//! real `kb` binary via assert_cmd — this is also, transitively, coverage
//! for the "fetch ahead of every early return" half of the fix, since a
//! passing run only proves the fields showed up in the FULL-diff JSON
//! path (the two resolved versions differ here), not just the
//! "no recorded change" short-circuit.

use assert_cmd::Command;
use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

fn git(repo: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=t@kb",
            "-c",
            "user.name=kb-test",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git {args:?} failed");
}

fn commit_ts(repo: &Path) -> i64 {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["log", "-1", "--format=%ct"])
        .output()
        .expect("git log");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("commit timestamp parses")
}

/// Boot a git-versioned "smoke" kb with two commits, and pre-seed the
/// MI-W2.4c tombstone-era marker with a far-future `started_unix` — any
/// `--between` window built from the commit timestamps below (which are
/// "now", give or take test runtime) will predate it, so `epoch_caveat`
/// deterministically fires without needing a real forget/purge history.
async fn boot_git_kb_with_future_era() -> (tempfile::TempDir, SocketAddr, String, i64, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let file = repo.join("doc.html");
    std::fs::write(
        &file,
        "<html><body><h1>Doc</h1><p>First revision text.</p></body></html>",
    )
    .unwrap();
    git(&repo, &["add", "doc.html"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let first_ts = commit_ts(&repo);

    std::fs::write(
        &file,
        "<html><body><h1>Doc</h1><p>Second revision text.</p></body></html>",
    )
    .unwrap();
    git(&repo, &["add", "doc.html"]);
    git(&repo, &["commit", "-q", "-m", "second"]);
    let second_ts = commit_ts(&repo);

    let daemon_name = format!(
        "cli-diffbtw-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: repo.clone(),
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: None,
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: Some("git".into()),
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

    // MI-W2.4c — pre-seed the epoch-honesty marker at a FAR-FUTURE
    // timestamp (`ensure_tombstone_era` is idempotent read-or-create: an
    // existing file's value is returned verbatim, never overwritten) so
    // both commit timestamps — however close to "now" the test actually
    // runs — deterministically predate it.
    std::fs::create_dir_all(&paths.state).unwrap();
    std::fs::write(
        paths.tombstone_era_file(),
        serde_json::json!({ "started_unix": 9_999_999_999i64 }).to_string(),
    )
    .unwrap();

    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(800)).await;
    let id = kb_core::ids::ArtifactId::from_path("doc.html")
        .as_str()
        .to_string();
    (tmp, addr, id, first_ts, second_ts)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_between_json_carries_epoch_caveat_fields() {
    let (_tmp, addr, id, first_ts, second_ts) = boot_git_kb_with_future_era().await;
    let daemon = format!("http://{addr}");

    // d1 resolves to the "first" commit (at-or-before its own ts); d2 is
    // comfortably after the "second" commit — resolves to the newer
    // version (working tree or "second"), so the two sides genuinely
    // differ and the request exercises the FULL diff path (not the
    // "no recorded change" short-circuit).
    let d1 = chrono::DateTime::from_timestamp(first_ts, 0)
        .unwrap()
        .to_rfc3339();
    let d2 = chrono::DateTime::from_timestamp(second_ts + 60, 0)
        .unwrap()
        .to_rfc3339();

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "diff",
            &id,
            "--between",
            &d1,
            &d2,
            "--json",
            "--kb",
            "smoke",
            "--daemon",
            &daemon,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    let body: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("diff --between --json: {text}\n{e}"));

    assert_eq!(
        body["tombstone_era_started_unix"], 9_999_999_999i64,
        "full body: {body}"
    );
    assert_eq!(body["epoch_caveat"], true, "full body: {body}");
    // The underlying diff payload is still intact alongside the merged
    // epoch fields — the fix must not clobber the daemon's own response.
    assert!(body.get("from").is_some());
    assert!(body.get("to").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_between_json_no_recorded_change_still_carries_epoch_caveat_fields() {
    let (_tmp, addr, id, _first_ts, second_ts) = boot_git_kb_with_future_era().await;
    let daemon = format!("http://{addr}");

    // Both dates resolve to the SAME version (comfortably after the last
    // commit) — the "no recorded change" short-circuit, which used to
    // return before the epoch-honesty fetch ran at all (in EITHER output
    // mode). Must still carry the fields in `--json`.
    let same = chrono::DateTime::from_timestamp(second_ts + 60, 0)
        .unwrap()
        .to_rfc3339();

    let out = Command::cargo_bin("kb")
        .unwrap()
        .args([
            "diff",
            &id,
            "--between",
            &same,
            &same,
            "--json",
            "--kb",
            "smoke",
            "--daemon",
            &daemon,
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    let body: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("diff --between --json: {text}\n{e}"));

    assert_eq!(body["message"], "no recorded change between d1 and d2");
    assert_eq!(
        body["tombstone_era_started_unix"], 9_999_999_999i64,
        "full body: {body}"
    );
    assert_eq!(body["epoch_caveat"], true, "full body: {body}");
}
