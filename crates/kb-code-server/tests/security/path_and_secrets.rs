//! SEC-13 — path containment and the server-enforced secret denylist,
//! exercised over the wire on the directly-addressable read route
//! (`GET /api/file`) and the agent content lane (`GET /api/pack`).
//!
//! The finding these pin: `safe_rel_path` is LEXICAL, and
//! `repo.path.join(...)` + `std::fs::read` FOLLOWS SYMLINKS — so a
//! committed `docs/escape -> /outside` made an escape-free request read
//! outside the repo, and `.env`/`config/master.key` were readable by
//! anyone who knew the path (which v7's working-tree tree makes everyone).

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, SecuritySection};
use kb_core::paths::KbPaths;
use std::path::Path;

use crate::common::git;

/// A repo carrying: one ordinary file, one `.env`, one
/// `config/master.key`, and (on unix) one symlink pointing OUT.
fn fixture_repo(outside: &Path) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
    std::fs::write(dir.join(".env"), "DATABASE_URL=postgres://prod\n").unwrap();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::write(dir.join("config/master.key"), "0123456789abcdef\n").unwrap();
    std::fs::write(dir.join("secrets.enc"), "ciphertext\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside, dir.join("escape")).unwrap();
    #[cfg(not(unix))]
    let _ = outside;
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

async fn boot(path: &Path, security: SecuritySection) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        security,
        repos: vec![RepoEntry {
            name: "fx".to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

async fn get_file(base: &str, path: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{base}/api/file"))
        .query(&[("repo", "fx"), ("path", path)])
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn an_ordinary_file_still_reads() {
    let outside = tempfile::tempdir().unwrap();
    let repo = fixture_repo(outside.path());
    let (_tmp, base) = boot(repo.path(), SecuritySection::default()).await;
    let resp = get_file(&base, "a.txt").await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["content"].as_str(), Some("hello\n"));
    // The additive hint field is present and false for ordinary content.
    assert_eq!(body["redaction_hint"].as_bool(), Some(false));
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_escape_is_refused_with_the_typed_403() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("id_dsa"), b"PRIVATE\n").unwrap();
    let repo = fixture_repo(outside.path());
    let (_tmp, base) = boot(repo.path(), SecuritySection::default()).await;

    // Lexically clean (`..`-free, relative), and the symlink is a real,
    // committed repo entry — the pre-V70-A2 read succeeded.
    let resp = get_file(&base, "escape/id_dsa").await;
    assert_eq!(resp.status(), 403);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["type"].as_str(),
        Some("urn:kb:errors:path-outside-repo"),
        "body: {body}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn the_lexical_gate_still_fires_first_for_a_dot_dot_path() {
    // `safe_rel_path` stays the FIRST check (SEC-13's fix says so), so a
    // `..` path is still the pre-existing 400, not the new 403.
    let outside = tempfile::tempdir().unwrap();
    let repo = fixture_repo(outside.path());
    let (_tmp, base) = boot(repo.path(), SecuritySection::default()).await;
    let resp = get_file(&base, "../outside.txt").await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn denylisted_paths_are_refused_and_name_the_pattern_not_the_bytes() {
    let outside = tempfile::tempdir().unwrap();
    let repo = fixture_repo(outside.path());
    let (_tmp, base) = boot(repo.path(), SecuritySection::default()).await;

    for (path, pattern) in [(".env", ".env"), ("config/master.key", "*.key")] {
        let resp = get_file(&base, path).await;
        assert_eq!(resp.status(), 403, "{path}");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(
            body["type"].as_str(),
            Some("urn:kb:errors:redacted-by-policy"),
            "{path} body: {body}"
        );
        let msg = body["error"].as_str().unwrap_or_default();
        assert!(msg.contains(pattern), "{path}: {msg}");
        // The refusal must NEVER carry the file's contents.
        assert!(!msg.contains("DATABASE_URL"), "{path}: {msg}");
        assert!(!msg.contains("0123456789abcdef"), "{path}: {msg}");
        assert!(body.get("content").is_none(), "{path}: {body}");
    }
}

#[tokio::test]
async fn the_denylist_applies_to_a_ref_read_too() {
    // A `.env` in `HEAD` is the same secret as a `.env` on disk — the
    // policy is checked before the branch, so `?ref=` gets it too.
    let outside = tempfile::tempdir().unwrap();
    let repo = fixture_repo(outside.path());
    let (_tmp, base) = boot(repo.path(), SecuritySection::default()).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/file"))
        .query(&[("repo", "fx"), ("path", ".env"), ("ref", "HEAD")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn config_secret_globs_are_additive() {
    let outside = tempfile::tempdir().unwrap();
    let repo = fixture_repo(outside.path());
    // `secrets.enc` is NOT in the built-in floor...
    let (_tmp, base) = boot(repo.path(), SecuritySection::default()).await;
    assert_eq!(get_file(&base, "secrets.enc").await.status(), 200);

    // ...until the operator adds it, and the floor is unchanged.
    let (_tmp2, base2) = boot(
        repo.path(),
        SecuritySection {
            secret_globs: vec!["*.enc".to_string()],
            ..Default::default()
        },
    )
    .await;
    assert_eq!(get_file(&base2, "secrets.enc").await.status(), 403);
    assert_eq!(get_file(&base2, ".env").await.status(), 403);
    assert_eq!(get_file(&base2, "a.txt").await.status(), 200);
}

#[tokio::test]
async fn the_agent_content_lane_is_covered_by_the_same_policy() {
    // `GET /api/pack` returns file CONTENT to an agent, which then writes
    // it into a transcript — the critique's MISSING #5. It reads through
    // `agentview::read_working_tree_file`, which carries the same ladder.
    let outside = tempfile::tempdir().unwrap();
    let repo = fixture_repo(outside.path());
    let (_tmp, base) = boot(repo.path(), SecuritySection::default()).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/pack"))
        .query(&[("repo", "fx"), ("paths", ".env")])
        .send()
        .await
        .unwrap();
    let body = resp.text().await.unwrap();
    assert!(
        !body.contains("DATABASE_URL"),
        "pack must never carry denylisted bytes: {body}"
    );
}

#[tokio::test]
async fn the_redaction_hint_is_set_on_credential_shaped_text_and_nothing_is_withheld() {
    let outside = tempfile::tempdir().unwrap();
    let repo = fixture_repo(outside.path());
    // A file that is NOT denylisted but LOOKS like it carries a secret.
    std::fs::write(
        repo.path().join("settings.yml"),
        "host: db.internal\napi_key: 91acf30bd77e4b6a9c11\n",
    )
    .unwrap();
    let (_tmp, base) = boot(repo.path(), SecuritySection::default()).await;
    let resp = get_file(&base, "settings.yml").await;
    assert_eq!(resp.status(), 200, "a hint must never withhold the bytes");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["redaction_hint"].as_bool(), Some(true));
    assert!(body["content"]
        .as_str()
        .unwrap()
        .contains("91acf30bd77e4b6a9c11"));
}
