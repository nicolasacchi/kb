//! V76-R4f — `[kb_daemon]` defaults to DISABLED unless the operator
//! configures `url` (E6 footgun fix: a throwaway kb-code daemon must never
//! federate against a live kb it was never pointed at — see
//! `kb_code_server::config::KbDaemonSection`'s struct doc for the full
//! resolution table).
//!
//! This is the ONE end-to-end HTTP test named in the unit brief: boot a
//! real daemon from a `kb-code.toml` string with **no `[kb_daemon]` section
//! at all** — the real `KbCodeConfig::from_toml_str` parse path, not a
//! hand-built `KbDaemonSection::default()` literal, so a regression in
//! `RawKbDaemonSection`'s `TryFrom` resolution would fail this test too —
//! and prove every kb-federated surface still answers `200`, degrading
//! honestly (an additive `kb-disabled`/`kb_daemon_enabled: false` signal),
//! never a silent empty and never a `500`.

use crate::common::{git, init_repo};
use kb_code_server::config::KbCodeConfig;
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// One commit at a controlled author date — mirrors
/// `provenance_routes.rs`'s own `commit` helper (each file in this test
/// binary grows its own minimal copy, per that file's module doc).
fn commit(dir: &Path, file: &str, contents: &str, message: &str, author_unix: i64) {
    std::fs::write(dir.join(file), contents).unwrap();
    git(dir, &["add", "-A"]);
    let date = format!("{author_unix} +0000");
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .status()
        .expect("git commit runs");
    assert!(status.success());
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// Boots straight off a PARSED `kb-code.toml` string carrying `[[repos]]`
/// and nothing else — no `[kb_daemon]` key anywhere.
async fn boot_with_no_kb_daemon_section(repo_name: &str, repo_dir: &Path) -> Boot {
    let canon = std::fs::canonicalize(repo_dir).unwrap();
    let toml = format!(
        "[[repos]]\nname = \"{repo_name}\"\npath = \"{}\"\n",
        canon.display()
    );
    let cfg = KbCodeConfig::from_toml_str(&toml).expect("a [kb_daemon]-less toml must parse");
    // Pin the precondition this whole test exists to check — if a future
    // change reintroduces a default `enabled = true`, this assertion fails
    // right here rather than the HTTP asserts below failing confusingly.
    assert!(
        !cfg.kb_daemon.enabled,
        "an absent [kb_daemon] section must resolve to disabled (V76-R4f)"
    );
    assert!(cfg.kb_daemon.url.is_none());

    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

/// Waits for `GET /api/file` to read `path` back — see
/// `provenance_routes.rs::wait_for_indexed`'s doc for why this only proves
/// working-tree readability, not indexing; fine here since this test never
/// asserts on `symbols`.
async fn wait_for_indexed(base: &str, repo: &str, path: &str) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(resp) = client
            .get(format!("{base}/api/file"))
            .query(&[("repo", repo), ("path", path)])
            .send()
            .await
        {
            if resp.status().is_success() {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "expected {path} to be readable from the working tree"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_kb_daemon_section_degrades_honestly_across_identity_repos_and_why() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(
        dir,
        "f.rs",
        "fn a() {}\n",
        "an unremarkable commit",
        1_700_000_000,
    );

    let boot = boot_with_no_kb_daemon_section("fixture", dir).await;
    wait_for_indexed(&boot.base, "fixture", "f.rs").await;

    let client = reqwest::Client::new();

    // GET /api/identity — 200; the additive V76-R4f field says disabled,
    // and the link base is an honest "" rather than a guessed address.
    let identity_resp = client
        .get(format!("{}/api/identity", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(identity_resp.status(), 200);
    let identity: serde_json::Value = identity_resp.json().await.unwrap();
    assert_eq!(identity["kb_daemon_enabled"], false, "body: {identity}");
    assert_eq!(identity["kb_public_url"], "", "body: {identity}");

    // GET /api/repos — 200 regardless; this route has no kb_daemon
    // dependency at all, so it is unaffected either way (asserted anyway,
    // per the unit brief).
    let repos_resp = client
        .get(format!("{}/api/repos", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(repos_resp.status(), 200);

    // GET /api/why — the provenance read: 200, with the join ladder's own
    // pre-existing "kb-disabled" attribution reason (`join::ladder::
    // VIA_KB_DISABLED`) rather than a 500 or a silently-empty attribution.
    let why_resp = client
        .get(format!("{}/api/why", boot.base))
        .query(&[("repo", "fixture"), ("path", "f.rs"), ("line", "1")])
        .send()
        .await
        .unwrap();
    assert_eq!(why_resp.status(), 200);
    let why: serde_json::Value = why_resp.json().await.unwrap();
    assert_eq!(why["attribution"]["via"], "kb-disabled", "body: {why}");
    assert!(why["kb_context"].is_null(), "body: {why}");
}
