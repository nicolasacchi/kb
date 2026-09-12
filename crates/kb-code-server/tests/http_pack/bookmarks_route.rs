//! Phase N — end-to-end HTTP tests for `GET`/`POST /api/bookmarks`,
//! `PATCH`/`DELETE /api/bookmarks/{id}` against a real daemon booted via
//! `serve_on_random_port_with_paths`. Mirrors `tests/reading_sets_route.rs`.

use crate::common::git;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("lib.rs"), "fn a() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: Some("http://127.0.0.1:0".to_string()),
        token_file: None,
        public_url: None,
    }
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot_with_repo(name: &str, path: &Path) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        ..KbCodeConfig::default()
    };
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bookmarks_crud_happy_path() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let create = client
        .post(format!("{}/api/bookmarks", boot.base))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "lib.rs",
            "line": 1,
            "note": "entry",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = create.json().await.unwrap();
    assert_eq!(created["path"], "lib.rs");
    assert_eq!(created["line"], 1);
    assert_eq!(created["note"], "entry");
    let id = created["id"].as_i64().unwrap();

    let list: serde_json::Value = client
        .get(format!("{}/api/bookmarks", boot.base))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["schema"], "bookmarks/1");
    assert_eq!(list["bookmarks"].as_array().unwrap().len(), 1);

    let patch = client
        .patch(format!("{}/api/bookmarks/{id}", boot.base))
        .json(&serde_json::json!({ "line": 2, "note": "moved" }))
        .send()
        .await
        .unwrap();
    assert!(patch.status().is_success());
    let patched: serde_json::Value = patch.json().await.unwrap();
    assert_eq!(patched["line"], 2);
    assert_eq!(patched["note"], "moved");

    let del = client
        .delete(format!("{}/api/bookmarks/{id}", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), reqwest::StatusCode::NO_CONTENT);

    let list2: serde_json::Value = client
        .get(format!("{}/api/bookmarks", boot.base))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list2["bookmarks"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mnemonic_move_semantics() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let a = client
        .post(format!("{}/api/bookmarks", boot.base))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 1, "mnemonic": "a",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(a.status(), reqwest::StatusCode::CREATED);
    let a_body: serde_json::Value = a.json().await.unwrap();
    let a_id = a_body["id"].as_i64().unwrap();

    // Reassign mnemonic `a` to a new bookmark — old owner is deleted.
    let b = client
        .post(format!("{}/api/bookmarks", boot.base))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 3, "mnemonic": "a", "note": "new",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(b.status(), reqwest::StatusCode::CREATED);
    let b_body: serde_json::Value = b.json().await.unwrap();
    let b_id = b_body["id"].as_i64().unwrap();
    assert_ne!(a_id, b_id);

    let list: serde_json::Value = client
        .get(format!("{}/api/bookmarks", boot.base))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let marks = list["bookmarks"].as_array().unwrap();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0]["id"], b_id);
    assert_eq!(marks[0]["mnemonic"], "a");
    assert_eq!(marks[0]["line"], 3);

    // Old id is gone.
    let gone = client
        .get(format!("{}/api/bookmarks", boot.base))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(!gone["bookmarks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == a_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_mnemonic_is_422() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    for bad in ["AB", "A", "_", "", "ab"] {
        let resp = client
            .post(format!("{}/api/bookmarks", boot.base))
            .json(&serde_json::json!({
                "repo": "r", "path": "lib.rs", "line": 1, "mnemonic": bad,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "mnemonic {bad:?}"
        );
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(
            body["error"].as_str().unwrap_or("").contains("mnemonic"),
            "{body}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_id_is_404_and_unknown_repo_is_404() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let missing = client
        .delete(format!("{}/api/bookmarks/99999", boot.base))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let patch = client
        .patch(format!("{}/api/bookmarks/99999", boot.base))
        .json(&serde_json::json!({ "line": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(patch.status(), reqwest::StatusCode::NOT_FOUND);

    let bad_repo = client
        .get(format!("{}/api/bookmarks", boot.base))
        .query(&[("repo", "nope")])
        .send()
        .await
        .unwrap();
    assert_eq!(bad_repo.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_ordering_mnemonic_first_then_created_at() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    // Create anonymous first, then mnemonic `b`, then mnemonic `a`.
    for body in [
        serde_json::json!({"repo":"r","path":"lib.rs","line":1,"note":"anon1"}),
        serde_json::json!({"repo":"r","path":"lib.rs","line":2,"mnemonic":"b"}),
        serde_json::json!({"repo":"r","path":"lib.rs","line":3,"mnemonic":"a"}),
        serde_json::json!({"repo":"r","path":"lib.rs","line":4,"note":"anon2"}),
    ] {
        let resp = client
            .post(format!("{}/api/bookmarks", boot.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    }

    let list: serde_json::Value = client
        .get(format!("{}/api/bookmarks", boot.base))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let marks = list["bookmarks"].as_array().unwrap();
    assert_eq!(marks.len(), 4);
    // Mnemonic-first ASC: a, b, then anonymous by created_at.
    assert_eq!(marks[0]["mnemonic"], "a");
    assert_eq!(marks[1]["mnemonic"], "b");
    assert!(marks[2]["mnemonic"].is_null());
    assert_eq!(marks[2]["note"], "anon1");
    assert!(marks[3]["mnemonic"].is_null());
    assert_eq!(marks[3]["note"], "anon2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn patch_clear_mnemonic_with_null() {
    let repo_tmp = fixture_repo();
    let boot = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    let created: serde_json::Value = client
        .post(format!("{}/api/bookmarks", boot.base))
        .json(&serde_json::json!({
            "repo": "r", "path": "lib.rs", "line": 1, "mnemonic": "x",
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();

    let patched: serde_json::Value = client
        .patch(format!("{}/api/bookmarks/{id}", boot.base))
        .json(&serde_json::json!({ "mnemonic": null }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(patched["mnemonic"].is_null(), "{patched}");
}
