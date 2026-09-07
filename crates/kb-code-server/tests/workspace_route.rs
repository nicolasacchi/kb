//! V75-M1 — end-to-end tests for D13's Workspace identity over a REAL git
//! repository with REAL linked worktrees, plus the `@ref` frame table.
//!
//! These live here rather than in `src/` for one concrete reason: building
//! the fixture means spawning `git worktree add`, and `tests/security/
//! git_argv_lint.rs` pins the set of files under `src/` allowed to spawn
//! `git`. `crate::workspace` deliberately spawns none (every read goes
//! through `history::run_git_raw`), and keeping its fixture out of `src/`
//! is what lets that stay true.
//!
//! What they cover that a unit test cannot: that two checkouts of one
//! repository really do resolve to ONE workspace id, that a MOVED worktree
//! keeps its id, that a worktree outside every configured root is listed
//! "known, not mounted", and that a prunable entry is reported rather than
//! silently dropped.
//!
//! `TMPDIR` discipline: set `TMPDIR` in the environment before running —
//! `tempfile::tempdir()` honours it.

mod common;

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry};
use kb_core::paths::KbPaths;
use std::path::Path;
use tokio::sync::Mutex as AsyncMutex;

use crate::common::git;
static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

/// A main checkout plus `n` linked worktrees under `<tmp>/wt-<i>`, all of
/// ONE repository — the shape the whole re-key is about.
///
/// Returns the tempdir (whose `main/` is the primary checkout) so both the
/// main and the linked paths stay alive for the test.
fn fixture_workspace(linked: &[&str]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    common::init_repo(&main);
    std::fs::write(main.join("README.md"), "# fixture\n").unwrap();
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "root commit"]);
    for name in linked {
        let path = tmp.path().join(name);
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &format!("branch-{name}"),
                path.to_str().unwrap(),
            ],
        );
    }
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
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
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

/// The re-key resolution is a BACKGROUND pass (V72-B0(a): it walks git
/// history and must never sit between `Store::open` and the bind), so a
/// test that reads `/api/workspaces` has to wait for it exactly the way a
/// client would — by watching the honesty flag it publishes.
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

#[tokio::test]
async fn two_worktrees_of_one_repository_resolve_to_one_workspace() {
    let _g = SERIAL.lock().await;
    let fx = fixture_workspace(&["wt-feature"]);
    let main = fx.path().join("main");
    let feature = fx.path().join("wt-feature");
    // BOTH checkouts registered as separate `[[repos]]` entries — the
    // configuration the re-key exists for.
    let boot = boot_with_repos(&[("acme", &main), ("acme-feature", &feature)]).await;
    let body = wait_for_rekey(&boot.base).await;

    let workspaces = body["workspaces"].as_array().unwrap();
    assert_eq!(
        workspaces.len(),
        1,
        "two checkouts of one object store are ONE workspace, not two: {body:#}"
    );
    let ws = &workspaces[0];
    assert!(
        ws["id"].as_str().unwrap().starts_with("ws_"),
        "{:?}",
        ws["id"]
    );
    assert!(
        ws["root_commit"].as_str().is_some(),
        "the root commit resolved: {ws:#}"
    );
    let repos: Vec<&str> = ws["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert_eq!(repos, vec!["acme", "acme-feature"]);

    // Two worktrees, one main and one linked, both MOUNTED.
    let wts = ws["worktrees"].as_array().unwrap();
    assert_eq!(wts.len(), 2, "{ws:#}");
    let main_wt = wts.iter().find(|w| w["is_main"] == true).unwrap();
    assert_eq!(main_wt["id"], "(main)");
    assert_eq!(main_wt["path_resolution"], "exact");
    assert_eq!(main_wt["repo"], "acme");
    let linked = wts.iter().find(|w| w["is_main"] == false).unwrap();
    assert_eq!(
        linked["id"], "wt-feature",
        "a linked worktree's id is its ADMIN-DIR name, which git derives from the path \
         basename: {linked:#}"
    );
    assert_eq!(linked["branch"], "branch-wt-feature");
    assert_eq!(linked["path_resolution"], "exact");
    assert_eq!(linked["repo"], "acme-feature");
    assert_eq!(linked["mounted"], true);
}

/// D13's "path is a mutable attribute", proven: the row's `path` moves and
/// its `id` does not.
#[tokio::test]
async fn a_moved_worktree_keeps_its_id_and_changes_only_its_path() {
    let _g = SERIAL.lock().await;
    let fx = fixture_workspace(&["wt-feature"]);
    let main = fx.path().join("main");
    let before = fx.path().join("wt-feature");
    let after = fx.path().join("wt-moved");

    let boot = boot_with_repos(&[("acme", &main)]).await;
    let body = wait_for_rekey(&boot.base).await;
    let ws0 = &body["workspaces"][0];
    let id0 = ws0["id"].as_str().unwrap().to_string();
    let linked0 = ws0["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["is_main"] == false)
        .unwrap()
        .clone();
    assert_eq!(linked0["id"], "wt-feature");
    assert_eq!(linked0["path"].as_str().unwrap(), before.to_str().unwrap());
    // Stop the first daemon before moving the tree under it: its watcher
    // is armed on this worktree and a live reconcile racing `git worktree
    // move` is a flake, not a feature under test.
    boot.task.abort();
    drop(boot);

    // `git worktree move` rewrites the admin `gitdir` file; the admin
    // DIRECTORY name — the id — is untouched.
    git(
        &main,
        &[
            "worktree",
            "move",
            before.to_str().unwrap(),
            after.to_str().unwrap(),
        ],
    );

    let boot = boot_with_repos(&[("acme", &main)]).await;
    let body = wait_for_rekey(&boot.base).await;
    let ws1 = &body["workspaces"][0];
    assert_eq!(
        ws1["id"].as_str().unwrap(),
        id0,
        "moving a worktree does not mint a second workspace"
    );
    let linked1 = ws1["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["is_main"] == false)
        .unwrap();
    assert_eq!(
        linked1["id"], "wt-feature",
        "the id is the admin-dir name and does not move with the path"
    );
    assert_eq!(linked1["path"].as_str().unwrap(), after.to_str().unwrap());
}

/// A worktree git knows about but that lies outside every configured root
/// is LISTED with that stated — never hidden, and never browsed.
#[tokio::test]
async fn a_worktree_outside_the_configured_roots_is_known_but_not_mounted() {
    let _g = SERIAL.lock().await;
    let fx = fixture_workspace(&["wt-outside"]);
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let body = wait_for_rekey(&boot.base).await;
    let ws = &body["workspaces"][0];
    let linked = ws["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["is_main"] == false)
        .unwrap();
    assert_eq!(linked["id"], "wt-outside");
    assert_eq!(
        linked["path_resolution"], "absent",
        "the sibling directory is not under the one configured root"
    );
    assert_eq!(linked["mounted"], false);
    assert_eq!(linked["repo"], serde_json::Value::Null);
}

/// A prunable entry (its directory deleted out from under git) is reported
/// with git's own reason. D13's thin slice says reads SKIP prunable
/// worktrees; it does not say hide them, and M2's `prune` verb needs
/// something to name.
#[tokio::test]
async fn a_prunable_worktree_is_reported_with_gits_own_reason() {
    let _g = SERIAL.lock().await;
    let fx = fixture_workspace(&["wt-gone"]);
    let main = fx.path().join("main");
    std::fs::remove_dir_all(fx.path().join("wt-gone")).unwrap();

    let boot = boot_with_repos(&[("acme", &main)]).await;
    let body = wait_for_rekey(&boot.base).await;
    let ws = &body["workspaces"][0];
    let linked = ws["worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["is_main"] == false)
        .expect("a prunable worktree is still listed");
    assert_eq!(linked["prunable"], true);
    assert!(
        linked["prunable_reason"].as_str().is_some(),
        "git's own reason rides the row: {linked:#}"
    );
    assert_eq!(linked["mounted"], false);
}

/// The narrowed read, and its 404.
#[tokio::test]
async fn the_per_workspace_read_narrows_and_404s_an_unknown_id() {
    let _g = SERIAL.lock().await;
    let fx = fixture_workspace(&[]);
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let body = wait_for_rekey(&boot.base).await;
    let id = body["workspaces"][0]["id"].as_str().unwrap().to_string();

    let (status, one) = get(&boot.base, &format!("/api/workspaces/{id}/worktrees")).await;
    assert_eq!(status, 200);
    assert_eq!(one["schema"], "kbc-workspace/1");
    assert_eq!(one["workspace"]["id"], id.as_str());
    assert_eq!(one["workspace"]["worktrees"].as_array().unwrap().len(), 1);

    let (status, _) = get(&boot.base, "/api/workspaces/ws_nope/worktrees").await;
    assert_eq!(status, 404);
}

/// `GET /api/repos` gains the two identities ADDITIVELY, and they are the
/// SAME values `/api/workspaces` reports — read off `repos`, never
/// recomputed, so the two surfaces cannot disagree.
#[tokio::test]
async fn the_repos_read_carries_the_same_identity_the_workspace_read_does() {
    let _g = SERIAL.lock().await;
    let fx = fixture_workspace(&["wt-feature"]);
    let main = fx.path().join("main");
    let boot = boot_with_repos(&[("acme", &main)]).await;
    let body = wait_for_rekey(&boot.base).await;
    let ws_id = body["workspaces"][0]["id"].as_str().unwrap().to_string();

    let (status, repos) = get(&boot.base, "/api/repos").await;
    assert_eq!(status, 200);
    let entry = &repos["repos"][0];
    assert_eq!(entry["name"], "acme");
    assert_eq!(entry["workspace_id"].as_str().unwrap(), ws_id);
    assert_eq!(entry["worktree_id"], "(main)");
    // Every pre-V75-M1 field is still there.
    for field in ["name", "path", "file_count", "writable", "is_worktree"] {
        assert!(!entry[field].is_null(), "{field} went missing");
    }

    let (_, identity) = get(&boot.base, "/api/identity").await;
    assert_eq!(identity["rekey"], "done");
}

/// The D14 frame table, over the wire, with the golden's own bytes as the
/// contract. `kb-code frames --json` prints exactly this.
#[tokio::test]
async fn the_frame_table_is_served_verbatim() {
    let _g = SERIAL.lock().await;
    let fx = fixture_workspace(&[]);
    let boot = boot_with_repos(&[("acme", &fx.path().join("main"))]).await;
    let (status, body) = get(&boot.base, "/api/frames").await;
    assert_eq!(status, 200);
    assert_eq!(body["schema"], "kbc-frames/1");

    let golden: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/frames.golden.json")).unwrap();
    assert_eq!(
        body, golden,
        "GET /api/frames must serve the golden table byte for byte — every off-HEAD banner \
         in the reader derives from it"
    );

    // The two sentences D14 is most often misread on, asserted through
    // the wire rather than through the const.
    let frames = body["frames"].as_array().unwrap();
    let lane = |name: &str| {
        frames
            .iter()
            .find(|f| f["lane"] == name)
            .unwrap_or_else(|| panic!("no {name} frame"))
            .clone()
    };
    assert_eq!(lane("lsp_live")["source"], "refused");
    assert_eq!(lane("blame")["source"], "git");
    assert_eq!(lane("tree")["source"], "odb");
    assert_eq!(lane("usages")["off_head"], "likely");
}
