//! V3.4-C1 — canvas-set HTTP CRUD + loopback-only mutation gate.
//!
//! Boot pattern mirrors `tests/local_review_routes.rs` /
//! `tests/reading_sets_route.rs`. Mutations (POST/PUT/DELETE) must 404 a
//! non-loopback caller — same XFF spoof technique as review mutations.

use crate::common::git;
use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), "base\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    tmp
}

async fn boot_with_repo(name: &str, path: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canvas_crud_list_get_update_delete_and_409() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    // Create
    let resp = client
        .post(format!("{base}/api/canvas"))
        .json(&serde_json::json!({
            "repo": "r",
            "name": "my-canvas",
            "review_id": 42,
            "payload": {"nodes": [{"id": "n1"}], "zoom": 1.0},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let created: serde_json::Value = resp.json().await.unwrap();
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["schema"], "canvas/1");
    assert_eq!(created["name"], "my-canvas");
    assert_eq!(created["review_id"], 42);
    assert_eq!(created["payload"]["zoom"], 1.0);
    assert!(created["created_unix"].as_i64().unwrap() > 0);
    assert_eq!(
        created["created_unix"].as_i64().unwrap(),
        created["updated_unix"].as_i64().unwrap()
    );

    // List (no payload body)
    let resp = client
        .get(format!("{base}/api/canvas"))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let item = &list["items"][0];
    assert_eq!(item["id"], id);
    assert_eq!(item["name"], "my-canvas");
    assert_eq!(item["review_id"], 42);
    assert!(item["payload"].is_null() || item.get("payload").is_none());
    assert!(item["payload_bytes"].as_i64().unwrap() > 0);

    // Get full
    let resp = client
        .get(format!("{base}/api/canvas/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(got["payload"]["nodes"][0]["id"], "n1");

    // Update payload + rename
    let resp = client
        .put(format!("{base}/api/canvas/{id}"))
        .json(&serde_json::json!({
            "payload": {"nodes": [], "zoom": 2.0},
            "name": "renamed",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["name"], "renamed");
    assert_eq!(updated["payload"]["zoom"], 2.0);
    assert!(updated["updated_unix"].as_i64().unwrap() >= created["updated_unix"].as_i64().unwrap());

    // 409 name collision
    let resp = client
        .post(format!("{base}/api/canvas"))
        .json(&serde_json::json!({
            "repo": "r",
            "name": "renamed",
            "payload": {},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409, "{}", resp.text().await.unwrap());

    // 413 payload over 256 KiB
    let pad = "x".repeat(256 * 1024); // well over after JSON overhead
    let resp = client
        .post(format!("{base}/api/canvas"))
        .json(&serde_json::json!({
            "repo": "r",
            "name": "huge",
            "payload": {"d": pad},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 413, "{}", resp.text().await.unwrap());
    let err: serde_json::Value = resp.json().await.unwrap();
    let msg = err["error"].as_str().unwrap_or("");
    assert!(
        msg.contains("bytes"),
        "413 body must report byte count: {msg}"
    );

    // Delete
    let resp = client
        .delete(format!("{base}/api/canvas/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204, "{}", resp.text().await.unwrap());

    let resp = client
        .get(format!("{base}/api/canvas/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    let list: serde_json::Value = client
        .get(format!("{base}/api/canvas"))
        .query(&[("repo", "r")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list["items"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canvas_mutations_404_for_non_loopback() {
    let _guard = SERIAL.lock().await;
    let repo_tmp = fixture_repo();
    let (_daemon_tmp, base) = boot_with_repo("r", repo_tmp.path()).await;
    let client = reqwest::Client::new();

    // Same technique as tests/local_review_routes.rs / tests/transcripts.rs:
    // XFF spoof with peer=loopback as a trusted hop makes is_loopback_origin
    // return false → 404.
    let resp = client
        .post(format!("{base}/api/canvas"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({
            "repo": "r",
            "name": "nope",
            "payload": {},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "POST /api/canvas must 404 a non-loopback caller"
    );

    // Seed one via loopback so PUT/DELETE have a target.
    let created: serde_json::Value = client
        .post(format!("{base}/api/canvas"))
        .json(&serde_json::json!({
            "repo": "r",
            "name": "seed",
            "payload": {"ok": true},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_i64().unwrap();

    let resp = client
        .put(format!("{base}/api/canvas/{id}"))
        .header("X-Forwarded-For", "8.8.8.8")
        .json(&serde_json::json!({"payload": {"x": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "PUT must 404 non-loopback");

    let resp = client
        .delete(format!("{base}/api/canvas/{id}"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "DELETE must 404 non-loopback");

    // GET still works for non-loopback (auth_bearer path — loopback bypass
    // still authenticates; XFF alone doesn't block reads).
    let resp = client
        .get(format!("{base}/api/canvas/{id}"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    // Without a bearer token and non-loopback, auth_bearer may 401 — either
    // 200 (if tests run with no token required) or 401 is acceptable for the
    // read path; the load-bearing assert is that it is NOT the mutation 404
    // from loopback_only. Default test boots have empty token → loopback
    // bypass still applies to auth for peer, but XFF makes origin non-loopback
    // → 401 when a token is configured. Our boot has no token + non-loopback
    // peer classification → 401 from fail-closed, OR 200 if allow_no_auth.
    // Accept any non-404 as "not the mutation gate".
    assert_ne!(
        resp.status(),
        404,
        "GET must not use the mutation loopback-only gate"
    );
}
