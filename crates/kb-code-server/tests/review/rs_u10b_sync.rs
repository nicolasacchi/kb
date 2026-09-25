//! RS-U10b — `POST /api/reviews/sync` and `GET /api/reviews/{id}/status`
//! end to end against a real daemon, a local bare "forge" and a mock
//! GitHub API (the `review_routes.rs` fixture shape: `origin` is CONFIGURED
//! as a github.com URL so `github_repo()` resolves acme/widget, and a
//! `url.<bare>.insteadOf` rewrite points the actual fetch at the local
//! bare repo). No store is seeded, so these drive the member-clone path;
//! the store path's sync semantics (rebase → GitHub-equal files, base-only
//! advance → unchanged, retarget) are pinned in-crate
//! (`src/review_sync/tests.rs`) against a ready review store. Every daemon
//! here runs with `[transcripts] enabled = false` and the kb daemon off.

use crate::common::{git, init_repo};
use axum::extract::Query;
use axum::routing::get;
use axum::{Json, Router};
use kb_code_server::config::{
    GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection, TranscriptsSection,
};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

const REPO: &str = "fixture";

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn cfg(path: &Path, gh_addr: SocketAddr) -> KbCodeConfig {
    KbCodeConfig {
        repos: vec![RepoEntry {
            name: REPO.to_string(),
            path: path.to_path_buf(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        review: ReviewSection::default(),
        transcripts: TranscriptsSection {
            enabled: false,
            root: "/nonexistent".into(),
            exclude_projects: Vec::new(),
            index_thinking: false,
        },
        github: GithubSection {
            token_file: None,
            api_base: format!("http://{gh_addr}"),
        },
        ..KbCodeConfig::default()
    }
}

async fn boot(cfg: KbCodeConfig) -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

// --- the fixture: a bare forge + a member clone ---------------------------------

struct Forge {
    _bare_tmp: tempfile::TempDir,
    _repo_tmp: tempfile::TempDir,
    bare: PathBuf,
    repo: PathBuf,
}

fn forge() -> Forge {
    let bare_tmp = tempfile::tempdir().unwrap();
    let bare = std::fs::canonicalize(bare_tmp.path())
        .unwrap()
        .join("origin.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(repo_tmp.path()).unwrap();
    init_repo(&repo);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widget.git",
        ],
    );
    git(
        &repo,
        &[
            "config",
            &format!("url.{}.insteadOf", bare.to_str().unwrap()),
            "https://github.com/acme/widget.git",
        ],
    );
    git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    Forge {
        _bare_tmp: bare_tmp,
        _repo_tmp: repo_tmp,
        bare,
        repo,
    }
}

impl Forge {
    /// Commit `files` on top of `from` and publish the tip as PR `n`'s
    /// head (forced, so a rewrite works too). Returns the tip.
    fn push_pr(&self, n: u32, from: &str, files: &[&str]) -> String {
        git(&self.repo, &["checkout", "-q", "--detach", from]);
        for f in files {
            std::fs::write(self.repo.join(f), format!("{f}\n")).unwrap();
            git(&self.repo, &["add", "-A"]);
            git(&self.repo, &["commit", "-q", "-m", f]);
        }
        let tip = git_out(&self.repo, &["rev-parse", "HEAD"]);
        git(
            &self.repo,
            &[
                "push",
                "-q",
                "-f",
                self.bare.to_str().unwrap(),
                &format!("HEAD:refs/pull/{n}/head"),
            ],
        );
        git(&self.repo, &["checkout", "-q", "main"]);
        tip
    }

    fn main_sha(&self) -> String {
        git_out(&self.repo, &["rev-parse", "main"])
    }
}

// --- the mock GitHub ------------------------------------------------------------

type Pulls = Arc<Mutex<HashMap<u64, Value>>>;

fn pull(n: u64, head: &str, changed_files: u64, state: &str, merged_at: Option<&str>) -> Value {
    json!({
        "number": n,
        "title": format!("PR {n} — à widget"),
        "user": { "login": "octocat" },
        "head": { "ref": format!("feature-{n}"), "sha": head },
        "base": { "ref": "main" },
        "updated_at": "2026-09-24T00:00:00Z",
        "draft": false,
        "state": state,
        "merged": merged_at.is_some(),
        "merged_at": merged_at,
        "labels": [],
        "mergeable_state": "clean",
        "changed_files": changed_files,
    })
}

async fn mock_github(pulls: Pulls) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let detail = pulls.clone();
    let list = pulls.clone();
    let router = Router::new()
        .route(
            "/repos/acme/widget/pulls/{n}",
            get(move |axum::extract::Path(n): axum::extract::Path<u64>| {
                let detail = detail.clone();
                async move {
                    match detail.lock().unwrap().get(&n).cloned() {
                        Some(v) => Ok(Json(v)),
                        None => Err(axum::http::StatusCode::NOT_FOUND),
                    }
                }
            }),
        )
        .route(
            "/repos/acme/widget/pulls",
            get(move |Query(q): Query<HashMap<String, String>>| {
                let list = list.clone();
                async move {
                    let want_closed = q.get("state").map(String::as_str) == Some("closed");
                    let mut rows: Vec<Value> = list
                        .lock()
                        .unwrap()
                        .values()
                        .filter(|p| (p["state"] == "closed") == want_closed)
                        .cloned()
                        .collect();
                    rows.sort_by_key(|p| p["number"].as_u64());
                    Json(Value::Array(rows))
                }
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

async fn sync(client: &reqwest::Client, base: &str, body: Value) -> (u16, Value) {
    let resp = client
        .post(format!("{base}/api/reviews/sync"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

async fn status(client: &reqwest::Client, base: &str, id: i64, fetch: bool) -> (u16, Value) {
    let mut req = client.get(format!("{base}/api/reviews/{id}/status"));
    if fetch {
        req = req.query(&[("fetch", "1")]);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

/// The documented `kbc-review-sync/1` keys and their JSON types.
fn assert_sync_shape(v: &Value) {
    let is_int_or_null = |x: &Value| x.is_i64() || x.is_u64() || x.is_null();
    assert_eq!(v["schema"], "kbc-review-sync/1", "{v:#}");
    assert!(v["repo"].is_string(), "{v:#}");
    assert!(v["pr_number"].is_u64(), "{v:#}");
    assert!(is_int_or_null(&v["review_id"]), "{v:#}");
    assert!(
        v["created"].is_boolean() && v["minted"].is_boolean(),
        "{v:#}"
    );
    assert!(is_int_or_null(&v["ps"]), "{v:#}");
    assert!(v["reason"].is_string(), "{v:#}");
    assert!(v["dry_run"].is_boolean(), "{v:#}");
    assert!(v["base"].is_object() || v["base"].is_null(), "{v:#}");
    for k in ["mode", "branch", "source", "state", "merge_base"] {
        if v["base"].is_object() {
            assert!(v["base"].get(k).is_some(), "base.{k} missing: {v:#}");
        }
    }
    assert!(
        v["head_sha"].is_string() || v["head_sha"].is_null(),
        "{v:#}"
    );
    assert!(is_int_or_null(&v["files_count"]), "{v:#}");
    for k in ["available", "state", "changed_files", "title", "base_ref"] {
        assert!(v["forge"].get(k).is_some(), "forge.{k} missing: {v:#}");
    }
    assert!(v["warnings"].is_array(), "{v:#}");
}

// --- tests ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_creates_then_is_idempotent_then_follows_pushes_and_merge_is_final() {
    let fx = forge();
    let m = fx.main_sha();
    let tip1 = fx.push_pr(7, &m, &["feature.txt"]);
    let pulls: Pulls = Arc::new(Mutex::new(HashMap::new()));
    pulls
        .lock()
        .unwrap()
        .insert(7, pull(7, &tip1, 1, "open", None));
    let (gh, _gh_task) = mock_github(pulls.clone()).await;
    let (_home, base) = boot(cfg(&fx.repo, gh)).await;
    let client = reqwest::Client::new();

    // 1. Created.
    let (st, v) = sync(&client, &base, json!({ "repo": REPO, "pr_number": 7 })).await;
    assert_eq!(st, 200, "{v:#}");
    assert_sync_shape(&v);
    assert_eq!(v["reason"], "created");
    assert_eq!(
        (v["created"].clone(), v["minted"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(v["ps"], 1);
    assert_eq!(v["head_sha"], tip1);
    assert_eq!(v["files_count"], 1);
    assert_eq!(v["forge"]["changed_files"], 1);
    assert_eq!(v["files_equal"], true, "GitHub-equal file count");
    assert_eq!(v["forge"]["state"], "open");
    let id = v["review_id"].as_i64().unwrap();

    // 2. Idempotent: nothing moved upstream.
    let (st, v) = sync(&client, &base, json!({ "repo": REPO, "pr_number": 7 })).await;
    assert_eq!(st, 200, "{v:#}");
    assert_sync_shape(&v);
    assert_eq!(v["reason"], "unchanged");
    assert_eq!(
        (v["created"].clone(), v["minted"].clone()),
        (json!(false), json!(false))
    );
    assert_eq!(
        (v["review_id"].as_i64(), v["ps"].as_i64()),
        (Some(id), Some(1))
    );

    // status: head current.
    let (st, s) = status(&client, &base, id, false).await;
    assert_eq!(st, 200, "{s:#}");
    assert_eq!(s["schema"], "kbc-review-status/1");
    assert_eq!(s["head_moved"], false);
    assert_eq!(s["remote_head"], tip1);
    assert_eq!(s["remote_head_source"], "forge-api");
    assert_eq!(s["latest_tip"], tip1);
    assert_eq!(s["drift"]["files_count"], 1);
    assert_eq!(s["drift"]["forge_changed_files"], 1);
    assert_eq!(s["drift"]["equal"], true);
    assert_eq!(s["verdict_stale"], false);
    assert_eq!(s["open_findings"], 0);
    assert!(s["base"].is_object());

    // 3. A push on the PR: status sees it BEFORE any sync (forge head vs
    //    the latest patchset tip, not the stored pr_head_sha)…
    let tip2 = fx.push_pr(7, &tip1, &["feature2.txt"]);
    pulls
        .lock()
        .unwrap()
        .insert(7, pull(7, &tip2, 2, "open", None));
    let (_, s) = status(&client, &base, id, false).await;
    assert_eq!(s["head_moved"], true, "{s:#}");
    assert_eq!(s["remote_head"], tip2);
    assert_eq!(s["latest_tip"], tip1);
    assert_eq!(s["drift"]["equal"], false);
    // `?fetch=1` with no ready store fetches nothing and says so.
    let (st, s) = status(&client, &base, id, true).await;
    assert_eq!(st, 200, "{s:#}");
    assert_eq!(s["fetched"], false);
    assert!(
        s["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "fetch-unavailable"),
        "{s:#}"
    );

    // …a dry run predicts it without writing…
    let (st, v) = sync(
        &client,
        &base,
        json!({ "repo": REPO, "pr_number": 7, "dry_run": true }),
    )
    .await;
    assert_eq!(st, 200, "{v:#}");
    assert_sync_shape(&v);
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["reason"], "head-moved");
    assert_eq!(v["minted"], true);
    assert_eq!(v["ps"], 1, "a dry run captures nothing");

    // …and the sync captures it: head-moved, GitHub-equal.
    let (st, v) = sync(&client, &base, json!({ "repo": REPO, "pr_number": 7 })).await;
    assert_eq!(st, 200, "{v:#}");
    assert_sync_shape(&v);
    assert_eq!(v["reason"], "head-moved");
    assert_eq!(
        (v["minted"].clone(), v["ps"].clone()),
        (json!(true), json!(2))
    );
    assert_eq!(v["head_sha"], tip2);
    assert_eq!(v["files_count"], 2);
    assert_eq!(v["files_equal"], true);
    let (_, s) = status(&client, &base, id, false).await;
    assert_eq!(s["head_moved"], false);

    // 4. The async job path: same body, polled through the start-pr job
    //    read; kind `sync`.
    let resp = client
        .post(format!("{base}/api/reviews/sync?async=1"))
        .json(&json!({ "repo": REPO, "pr_number": 7 }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 202);
    let job: Value = resp.json().await.unwrap();
    assert_eq!(job["kind"], "sync");
    let job_id = job["job_id"].as_str().unwrap().to_string();
    let mut done = Value::Null;
    for _ in 0..300 {
        let j: Value = client
            .get(format!("{base}/api/reviews/jobs/{job_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if j["status"] != "running" {
            done = j;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(done["status"], "done", "{done:#}");
    assert_eq!(done["kind"], "sync");
    assert_eq!(done["review_id"], id);
    assert_sync_shape(&done["result"]);
    assert_eq!(done["result"]["reason"], "unchanged");

    // 5. Merged: final — a later head is NOT captured.
    let tip3 = fx.push_pr(7, &tip2, &["feature3.txt"]);
    pulls
        .lock()
        .unwrap()
        .insert(7, pull(7, &tip3, 3, "closed", Some("2026-09-24T10:00:00Z")));
    let (st, v) = sync(&client, &base, json!({ "repo": REPO, "pr_number": 7 })).await;
    assert_eq!(st, 200, "{v:#}");
    assert_sync_shape(&v);
    assert_eq!(v["reason"], "merged-final");
    assert_eq!(v["minted"], false);
    assert_eq!(v["ps"], 2);
    assert_eq!(v["head_sha"], tip2);
    assert_eq!(v["forge"]["state"], "merged");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_open_runs_every_pr_and_reports_failures_in_line() {
    let fx = forge();
    let m = fx.main_sha();
    let t1 = fx.push_pr(1, &m, &["one.txt"]);
    let t2 = fx.push_pr(2, &m, &["two-a.txt", "two-b.txt"]);
    let t4 = fx.push_pr(4, &m, &["four.txt"]);
    let pulls: Pulls = Arc::new(Mutex::new(HashMap::new()));
    {
        let mut p = pulls.lock().unwrap();
        p.insert(1, pull(1, &t1, 1, "open", None));
        p.insert(2, pull(2, &t2, 2, "open", None));
        // Listed by the forge, but its head ref was never published:
        // this one fails.
        p.insert(3, pull(3, &"3".repeat(40), 1, "open", None));
        // Merged inside the window / before it.
        p.insert(4, pull(4, &t4, 1, "closed", Some("2026-09-24T10:00:00Z")));
        p.insert(5, pull(5, &t4, 1, "closed", Some("2026-09-01T10:00:00Z")));
    }
    let (gh, _gh_task) = mock_github(pulls.clone()).await;
    let (_home, base) = boot(cfg(&fx.repo, gh)).await;
    let client = reqwest::Client::new();

    let (st, v) = sync(
        &client,
        &base,
        json!({ "repo": REPO, "open": true, "merged_since": "2026-09-20" }),
    )
    .await;
    assert_eq!(st, 200, "{v:#}");
    assert_eq!(v["schema"], "kbc-review-sync-open/1");
    assert_eq!(v["count"], 4, "{v:#}");
    assert_eq!(v["failed"], 1, "{v:#}");
    let items = v["items"].as_array().unwrap();
    let by_pr: HashMap<u64, &Value> = items
        .iter()
        .map(|i| (i["pr_number"].as_u64().unwrap(), i))
        .collect();
    assert!(!by_pr.contains_key(&5), "merged before the window");
    for n in [1u64, 2, 4] {
        let it = by_pr[&n];
        assert_eq!(it["ok"], true, "{it:#}");
        assert_sync_shape(it);
        assert_eq!(it["reason"], "created");
        assert_eq!(it["files_equal"], true, "{it:#}");
    }
    assert_eq!(by_pr[&4]["listed_as"], "merged");
    assert_eq!(by_pr[&1]["listed_as"], "open");
    let bad = by_pr[&3];
    assert_eq!(bad["ok"], false);
    assert!(bad["error"]["code"]
        .as_str()
        .unwrap()
        .starts_with("urn:kb:errors:"));
    assert!(bad["error"]["message"]
        .as_str()
        .unwrap()
        .contains("PR fetch failed"));
    assert_eq!(bad["error"]["status"], 400);

    // Run again: the three good ones are quiet, PR 4 is merged-final.
    let (_, v) = sync(
        &client,
        &base,
        json!({ "repo": REPO, "open": true, "merged_since": "2026-09-20" }),
    )
    .await;
    let reasons: HashMap<u64, String> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["ok"] == true)
        .map(|i| {
            (
                i["pr_number"].as_u64().unwrap(),
                i["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(reasons[&1], "unchanged");
    assert_eq!(reasons[&2], "unchanged");
    assert_eq!(reasons[&4], "merged-final");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_refusals_are_typed() {
    let fx = forge();
    let pulls: Pulls = Arc::new(Mutex::new(HashMap::new()));
    let (gh, _gh_task) = mock_github(pulls).await;
    let (_home, base) = boot(cfg(&fx.repo, gh)).await;
    let client = reqwest::Client::new();

    for (body, want) in [
        (json!({ "repo": REPO }), 400),
        (json!({ "repo": REPO, "pr_number": 7, "open": true }), 400),
        (
            json!({ "repo": REPO, "pr_number": 7, "merged_since": "2026-09-20" }),
            400,
        ),
        (
            json!({ "repo": REPO, "open": true, "merged_since": "soon" }),
            400,
        ),
        (json!({ "repo": "nope", "pr_number": 7 }), 404),
    ] {
        let (st, v) = sync(&client, &base, body.clone()).await;
        assert_eq!(st, want, "{body} → {v:#}");
        assert!(v["error"].is_string(), "{v:#}");
    }
    // A PR the forge does not know and whose ref does not exist: the fetch
    // is the load-bearing step — a 400, no review row.
    let (st, v) = sync(&client, &base, json!({ "repo": REPO, "pr_number": 99 })).await;
    assert_eq!(st, 400, "{v:#}");
    assert!(
        v["error"].as_str().unwrap().contains("PR fetch failed"),
        "{v:#}"
    );
    let (st, _) = status(&client, &base, 424242, false).await;
    assert_eq!(st, 404);
}
