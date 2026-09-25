//! V76-R3b — lifecycle verbs, readiness, inbox lane, and the one oracle
//! over a real git repository with linked worktrees.
//!
//! Lives here (not in `src/worktrees.rs`) for the same reason
//! `workspace_route.rs` does: building the fixture means spawning
//! `git worktree add`, and the git-argv lint pins files under `src/`.

mod common;

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    common::init_repo(&main);
    std::fs::write(main.join("README.md"), "# fixture\n").unwrap();
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "root commit"]);
    git(&main, &["branch", "feature"]);
    let linked = tmp.path().join("wt-linked");
    git(
        &main,
        &["worktree", "add", "-q", linked.to_str().unwrap(), "feature"],
    );
    tmp
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot_with_repos(repos: &[(&str, &Path)]) -> Boot {
    let cfg = KbCodeConfig {
        repos: repos
            .iter()
            .map(|(name, path)| RepoEntry {
                name: name.to_string(),
                path: std::fs::canonicalize(path).unwrap(),
            })
            .collect(),
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
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
    }
}

async fn get(base: &str, path: &str) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}{path}"))
        .send()
        .await
        .expect("request");
    let status = resp.status().as_u16();
    let body = resp.json().await.unwrap_or(serde_json::Value::Null);
    (status, body)
}

async fn post(base: &str, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    let resp = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body)
        .send()
        .await
        .expect("request");
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::Null),
    )
}

async fn wait_for_rekey(base: &str) -> serde_json::Value {
    let empty: Vec<serde_json::Value> = Vec::new();
    for _ in 0..200 {
        let (_, body) = get(base, "/api/workspaces").await;
        let listed = body["workspaces"].as_array().unwrap_or(&empty).len();
        if body["rekey"] == "done" && listed > 0 {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("the re-key never reported done");
}

fn workspace_id(body: &serde_json::Value) -> String {
    body["workspaces"][0]["id"].as_str().unwrap().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_under_root_succeeds_and_outside_is_refused_by_name() {
    let _g = SERIAL.lock().await;
    let fx = fixture();
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let ws_body = wait_for_rekey(&boot.base).await;
    let ws = workspace_id(&ws_body);

    let inside = fx.path().join("wt-created");
    let (status, body) = post(
        &boot.base,
        "/api/worktrees",
        serde_json::json!({
            "workspace_id": ws,
            "path": inside.to_str().unwrap(),
            "new_branch": "created-branch",
        }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["after"]["created_by_daemon"], true);
    assert!(inside.join(".git").is_file());

    let outside = tempfile::tempdir().unwrap();
    let nope = outside.path().join("nope");
    let (status, body) = post(
        &boot.base,
        "/api/worktrees",
        serde_json::json!({
            "workspace_id": ws,
            "path": nope.to_str().unwrap(),
            "new_branch": "outside-branch",
        }),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    let err = body["error"].as_str().unwrap_or("");
    assert!(err.contains(nope.to_str().unwrap()), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lock_unlock_and_audit_rows() {
    let _g = SERIAL.lock().await;
    let fx = fixture();
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let ws_body = wait_for_rekey(&boot.base).await;
    let ws = workspace_id(&ws_body);
    let created = fx.path().join("wt-lock");
    let (status, body) = post(
        &boot.base,
        "/api/worktrees",
        serde_json::json!({
            "workspace_id": ws,
            "path": created.to_str().unwrap(),
            "new_branch": "lock-branch",
        }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let id = body["after"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &boot.base,
        &format!("/api/worktrees/{id}/lock"),
        serde_json::json!({
            "workspace_id": ws,
            "reason": "claude/abcd1234 holding this",
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["after"]["locked"], true);
    assert_eq!(body["before"]["locked"], false);
    assert_eq!(body["after"]["owner"]["trust"], "likely");

    let (status, body) = post(
        &boot.base,
        &format!("/api/worktrees/{id}/unlock"),
        serde_json::json!({ "workspace_id": ws }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["after"]["locked"], false);

    let (status, audit) = get(&boot.base, "/api/audit?limit=20").await;
    assert_eq!(status, 200);
    let entries = audit["entries"].as_array().unwrap();
    assert!(
        entries.iter().any(
            |e| e["route"].as_str().unwrap_or("").contains("/api/worktrees")
                && e["method"] == "POST"
                && e["admission"] == "loopback"
        ),
        "{audit}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_is_refused_for_non_daemon_created_and_allowed_with_preview() {
    let _g = SERIAL.lock().await;
    let fx = fixture();
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let ws_body = wait_for_rekey(&boot.base).await;
    let ws = workspace_id(&ws_body);

    let linked_id = ws_body["workspaces"][0]["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] != "(main)")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = reqwest::Client::new()
        .delete(format!("{}/api/worktrees/{linked_id}", boot.base))
        .json(&serde_json::json!({
            "workspace_id": ws,
            "confirm": linked_id,
            "preview_seen": true,
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 403, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("git worktree remove --"),
        "{body}"
    );

    let created = fx.path().join("wt-rm");
    let (status, body) = post(
        &boot.base,
        "/api/worktrees",
        serde_json::json!({
            "workspace_id": ws,
            "path": created.to_str().unwrap(),
            "new_branch": "rm-branch",
        }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let id = body["after"]["id"].as_str().unwrap().to_string();

    let (status, preview) = get(
        &boot.base,
        &format!("/api/worktrees/{id}/loss-preview?workspace_id={ws}"),
    )
    .await;
    assert_eq!(status, 200, "{preview}");
    assert!(preview["uncommitted"].is_array());
    assert!(preview["unpushed"].is_array());
    assert!(preview["stashes"].is_array());
    assert_eq!(preview["created_by_daemon"], true);

    let resp = reqwest::Client::new()
        .delete(format!("{}/api/worktrees/{id}", boot.base))
        .json(&serde_json::json!({
            "workspace_id": ws,
            "confirm": id,
            "preview_seen": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        200,
        "{}",
        resp.text().await.unwrap()
    );
    assert!(!created.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prune_dry_run_lists_and_apply_removes() {
    let _g = SERIAL.lock().await;
    let fx = fixture();
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let ws_body = wait_for_rekey(&boot.base).await;
    let ws = workspace_id(&ws_body);
    let created = fx.path().join("wt-prune");
    let (status, body) = post(
        &boot.base,
        "/api/worktrees",
        serde_json::json!({
            "workspace_id": ws,
            "path": created.to_str().unwrap(),
            "new_branch": "prune-branch",
        }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    std::fs::remove_dir_all(&created).unwrap();

    let (status, body) = post(
        &boot.base,
        "/api/worktrees/prune?dry_run=1",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dry_run"], true);

    let (status, body) = post(
        &boot.base,
        "/api/worktrees/prune?dry_run=0",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["dry_run"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_after_a_moved_worktree_and_readiness_emits_commands() {
    let _g = SERIAL.lock().await;
    let fx = fixture();
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let ws_body = wait_for_rekey(&boot.base).await;
    let ws = workspace_id(&ws_body);
    let created = fx.path().join("wt-repair");
    let (status, body) = post(
        &boot.base,
        "/api/worktrees",
        serde_json::json!({
            "workspace_id": ws,
            "path": created.to_str().unwrap(),
            "new_branch": "repair-branch",
        }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let id = body["after"]["id"].as_str().unwrap().to_string();

    let moved = fx.path().join("wt-repair-moved");
    std::fs::rename(&created, &moved).unwrap();
    let (status, body) = post(
        &boot.base,
        &format!("/api/worktrees/{id}/repair"),
        serde_json::json!({
            "workspace_id": ws,
            "path": moved.to_str().unwrap(),
        }),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, ready) = get(
        &boot.base,
        &format!("/api/worktrees/{id}/readiness?workspace_id={ws}"),
    )
    .await;
    assert_eq!(status, 200, "{ready}");
    assert!(ready["issues"].is_array());
}

// --- RS-U8: worktree materialize from the review store, by sha ------------

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// `for-each-ref` of `dir` — never lists `HEAD`, so byte-identical
/// before/after is exactly "only HEAD (or the requested branch) moved, no
/// ref was written" (BUILD-BRIEF U8's invariance test).
fn ref_tree(dir: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["for-each-ref", "--format=%(refname) %(objectname)"])
        .output()
        .expect("git runs");
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_from_a_store_only_review_tip_fetches_by_sha_and_writes_no_refs_kbc() {
    let _g = SERIAL.lock().await;
    let fx = fixture();
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let ws_body = wait_for_rekey(&boot.base).await;
    let ws = workspace_id(&ws_body);

    // A commit that will exist ONLY inside a kb-owned review store — an
    // unrelated repo, fetched into a bare store under a review-ref name
    // that never touches `main`'s own refs (ancestry doesn't matter:
    // `allowAnySHA1InWant` fetches by sha regardless).
    let src = fx.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    common::init_repo(&src);
    std::fs::write(src.join("b.txt"), "review tip\n").unwrap();
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "review tip"]);
    let tip = git_out(&src, &["rev-parse", "HEAD"]);

    let store_dir = fx.path().join("store.git");
    git(
        fx.path(),
        &["init", "-q", "--bare", store_dir.to_str().unwrap()],
    );
    std::fs::write(
        store_dir.join("config"),
        "[core]\n\trepositoryformatversion = 0\n\tbare = true\n\
         [uploadpack]\n\tallowAnySHA1InWant = true\n",
    )
    .unwrap();
    git(
        &store_dir,
        &[
            "fetch",
            "-q",
            src.to_str().unwrap(),
            "HEAD:refs/kbc/review/1/ps1",
        ],
    );

    // Register a `ready` review store for repo "acme" directly on the
    // daemon's own sqlite volume — the seeding job that would normally
    // produce this row is a separate unit; this test only needs the DB
    // shape a `ready` store leaves behind (the same precedent
    // `tests/review/local_review_routes.rs::verdict_zero_patchset_is_400`
    // uses for inserting a review row against a live daemon).
    let db = boot.tmp.path().join("state/kb-code/index.db");
    let store = kb_code_server::store::Store::open(&db).unwrap();
    let repo_id = store
        .upsert_repo(
            "acme",
            std::fs::canonicalize(&main).unwrap().to_str().unwrap(),
        )
        .unwrap();
    let store_id = store
        .create_review_store(
            "11111111-1111-1111-1111-111111111111",
            "local:acme-rs-u8-test",
            store_dir.to_str().unwrap(),
            None,
            None,
            1,
        )
        .unwrap();
    assert!(store
        .set_review_store_state(store_id, "ready", None)
        .unwrap());
    store.add_repo_to_store(repo_id, store_id).unwrap();
    // RS-U5 review fix — `GitCtx::for_repo` also requires this member's OWN
    // import to have landed (`repo_stores.legacy_import_json` set), the
    // same `MemberPending` check `ReviewStores::handle_for_repo` makes.
    // Without this, a `ready` store row still resolves to the work-tree
    // fallback for a member whose import never ran (this test's case).
    store
        .set_repo_store_legacy_import(
            repo_id,
            Some(r#"{"at":1,"imported":1,"conflicts":[],"missing_reviews":[]}"#),
        )
        .unwrap();
    drop(store);

    let before = ref_tree(&main);

    let created = fx.path().join("wt-review-tip");
    let (status, body) = post(
        &boot.base,
        "/api/worktrees",
        serde_json::json!({
            "workspace_id": ws,
            "path": created.to_str().unwrap(),
            "branch": tip,
        }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["after"]["detached"], true);

    // The linked worktree landed exactly at the review tip that, before
    // this call, existed nowhere in `main`'s own ODB...
    assert_eq!(git_out(&created, &["rev-parse", "HEAD"]), tip);
    // ...and `main`'s own ref tree — shared by every linked worktree of
    // this workspace — gained nothing: no `refs/kbc/*`, no ref of any
    // kind besides the worktree's own (ref-less, detached) HEAD.
    assert_eq!(
        ref_tree(&main),
        before,
        "worktree materialize via the store bridge must write no ref"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_worktrees_lane_degrades_when_empty_and_surfaces_when_ready() {
    let _g = SERIAL.lock().await;
    let fx = fixture();
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;

    let (status, body) = get(&boot.base, "/api/inbox").await;
    assert_eq!(status, 200, "{body}");
    assert!(body["worktrees"].is_object(), "{body}");
    // Either empty-table (re-key still pending) or available after it lands.
    let available = body["worktrees"]["available"].as_bool();
    assert!(available.is_some(), "{body}");
    if available == Some(false) {
        assert_eq!(body["worktrees"]["reason"], "empty-table");
    }

    let _ = wait_for_rekey(&boot.base).await;
    let (status, body) = get(&boot.base, "/api/inbox").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["worktrees"]["available"], true);
    assert!(body["worktrees"]["items"].is_array());
}
