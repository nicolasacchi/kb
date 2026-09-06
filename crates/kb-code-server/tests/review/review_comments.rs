//! V4.C1 — carry-forward resolution + `GET /api/reviews/{id}/comments`
//! auth. Real git fixtures (insert-above, delete, rebase, rename, range
//! normalize). Boot pattern mirrors `local_review_routes.rs`.

use kb_code_server::config::{KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;

use crate::common::git;

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

async fn boot_with_repo(
    name: &str,
    path: &Path,
    review: ReviewSection,
) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        review,
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

async fn create_review(
    client: &reqwest::Client,
    base: &str,
    repo: &str,
    head: &str,
    base_ref: &str,
) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": repo,
            "head_ref": head,
            "base_ref": base_ref,
            "title": "carry-forward",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn snapshot(client: &reqwest::Client, base: &str, id: i64) -> i64 {
    let resp = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json::<serde_json::Value>().await.unwrap()["ps_number"]
        .as_i64()
        .unwrap()
}

async fn comment_on(
    client: &reqwest::Client,
    base: &str,
    review_id: i64,
    path: &str,
    line: u32,
    body: &str,
    side: &str,
) -> serde_json::Value {
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": path,
            "line": line,
            "body": body,
            "review_id": review_id,
            "side": side,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json().await.unwrap()
}

async fn comments_at(client: &reqwest::Client, base: &str, id: i64, ps: &str) -> serde_json::Value {
    let resp = client
        .get(format!("{base}/api/reviews/{id}/comments"))
        .query(&[("ps", ps)])
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "{}",
        resp.text().await.unwrap()
    );
    resp.json().await.unwrap()
}

fn first_comment<'a>(body: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["path"] == path)
        .expect("path group")["comments"]
        .as_array()
        .unwrap()
        .first()
        .expect("comment")
}

fn comment_by_body<'a>(
    body: &'a serde_json::Value,
    path: &str,
    want: &str,
) -> &'a serde_json::Value {
    body["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["path"] == path)
        .expect("path group")["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == want)
        .expect("comment by body")
}

/// (a) insert lines above the anchored line → shifted, not orphaned.
/// (b) delete the anchored line → orphaned with original{ps,line,snippet}.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn carry_forward_shift_and_delete() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("lib.rs"),
        "alpha unique header\nTARGET unique comment line xyz\nomega unique footer\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("lib.rs"),
        "alpha unique header\nTARGET unique comment line xyz\nomega unique footer\nfeature extra\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature"]);

    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r", "feature", "main").await;

    let created = comment_on(&client, &base, id, "lib.rs", 2, "about TARGET", "new").await;
    assert_eq!(created["line"], 2);

    // Insert two unique lines ABOVE the target.
    std::fs::write(
        dir.join("lib.rs"),
        "alpha unique header\ninserted one\ninserted two\nTARGET unique comment line xyz\nomega unique footer\nfeature extra\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "insert above"]);
    assert_eq!(snapshot(&client, &base, id).await, 2);

    let listed = comments_at(&client, &base, id, "2").await;
    assert_eq!(listed["ps"], 2);
    let c = first_comment(&listed, "lib.rs");
    assert_eq!(c["body"], "about TARGET");
    assert_eq!(c["resolution"]["orphaned"], false);
    assert_eq!(c["resolution"]["line"], 4);
    assert_eq!(c["resolution"]["resolved_against"]["ps"], 2);
    assert!(c["suggestion"].is_null());

    // Delete the anchored line entirely (leave nothing similar).
    std::fs::write(
        dir.join("lib.rs"),
        "alpha unique header\ninserted one\ninserted two\nomega unique footer\nfeature extra\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "delete target"]);
    assert_eq!(snapshot(&client, &base, id).await, 3);

    let listed = comments_at(&client, &base, id, "3").await;
    let c = first_comment(&listed, "lib.rs");
    assert_eq!(c["resolution"]["orphaned"], true);
    assert_eq!(c["resolution"]["original"]["ps"], 1);
    assert_eq!(c["resolution"]["original"]["line"], 2);
    assert_eq!(
        c["resolution"]["original"]["snippet"],
        "TARGET unique comment line xyz"
    );
    assert_eq!(c["resolution"]["original"]["side"], "new");
}

/// (c) rebase: new-side re-resolves against the new tip; old-side
/// re-resolves against the NEW base blob; rewriting the anchored base
/// content orphans the old-side comment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn carry_forward_rebase_both_sides() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("base.rs"),
        "KEEP unique base line\nREWRITE unique base line\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "main-1"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("feat.rs"), "NEW SIDE unique feature line\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feat-1"]);

    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r", "feature", "main").await;

    comment_on(&client, &base, id, "feat.rs", 1, "new-side", "new").await;
    comment_on(&client, &base, id, "base.rs", 1, "old-keep", "old").await;
    comment_on(&client, &base, id, "base.rs", 2, "old-rewrite", "old").await;

    // Advance main: insert a line above KEEP, rewrite REWRITE.
    git(dir, &["checkout", "-q", "main"]);
    std::fs::write(
        dir.join("base.rs"),
        "inserted on main\nKEEP unique base line\ncompletely different base content now\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "advance main"]);

    git(dir, &["checkout", "-q", "feature"]);
    // rebase onto the new main — non-interactive.
    git(dir, &["rebase", "main"]);

    assert_eq!(snapshot(&client, &base, id).await, 2);

    let listed = comments_at(&client, &base, id, "2").await;
    let new_side = comment_by_body(&listed, "feat.rs", "new-side");
    assert_eq!(new_side["resolution"]["orphaned"], false);
    assert_eq!(new_side["resolution"]["line"], 1);
    assert_eq!(new_side["resolution"]["resolved_against"]["ps"], 2);

    let old_keep = comment_by_body(&listed, "base.rs", "old-keep");
    assert_eq!(old_keep["resolution"]["orphaned"], false);
    assert_eq!(
        old_keep["resolution"]["line"], 2,
        "KEEP shifted down by the main insert; old-side must re-resolve against the NEW base blob"
    );
    let new_base = git_out(dir, &["merge-base", "main", "HEAD"]);
    assert_eq!(
        old_keep["resolution"]["resolved_against"]["sha"]
            .as_str()
            .unwrap(),
        new_base.as_str(),
        "old-side must resolve against the new merge-base, not the original"
    );

    let old_rewrite = comment_by_body(&listed, "base.rs", "old-rewrite");
    assert_eq!(old_rewrite["resolution"]["orphaned"], true);
    assert_eq!(old_rewrite["resolution"]["original"]["ps"], 1);
    assert_eq!(old_rewrite["resolution"]["original"]["side"], "old");
    assert_eq!(old_rewrite["resolution"]["original"]["line"], 2);
    assert_eq!(
        old_rewrite["resolution"]["original"]["snippet"],
        "REWRITE unique base line"
    );
}

/// (d) file renamed away at the target ps → orphaned.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn carry_forward_renamed_file_is_orphaned() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("old.rs"), "UNIQUE rename target line\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("old.rs"),
        "UNIQUE rename target line\nfeature tweak\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feat"]);

    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r", "feature", "main").await;
    comment_on(&client, &base, id, "old.rs", 1, "about old.rs", "new").await;

    git(dir, &["mv", "old.rs", "new.rs"]);
    git(dir, &["commit", "-q", "-m", "rename"]);
    assert_eq!(snapshot(&client, &base, id).await, 2);

    let listed = comments_at(&client, &base, id, "2").await;
    let c = first_comment(&listed, "old.rs");
    assert_eq!(c["resolution"]["orphaned"], true);
    assert_eq!(c["resolution"]["original"]["ps"], 1);
    assert_eq!(
        c["resolution"]["original"]["snippet"],
        "UNIQUE rename target line"
    );
}

/// (e) range with crossed endpoints normalizes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn carry_forward_range_normalizes_crossed_endpoints() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(
        dir.join("r.rs"),
        "line one unique\nline two unique\nline three unique\nline four unique\nline five unique\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(
        dir.join("r.rs"),
        "line one unique\nline two unique\nline three unique\nline four unique\nline five unique\nextra\n",
    )
    .unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feat"]);

    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r", "feature", "main").await;

    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r",
            "path": "r.rs",
            "line": 5,
            "line_end": 2,
            "anchor_kind": "range",
            "body": "crossed",
            "review_id": id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "{}",
        resp.text().await.unwrap()
    );

    let listed = comments_at(&client, &base, id, "latest").await;
    let c = first_comment(&listed, "r.rs");
    assert_eq!(c["anchor_kind"], "range");
    assert_eq!(c["resolution"]["orphaned"], false);
    assert_eq!(c["resolution"]["line"], 2);
    assert_eq!(c["resolution"]["line_end"], 5);
}

/// Resolved threads are hidden by default and included with `all=true`.
/// Replies nest under the parent. Suggestion block is read-only (null
/// when no row).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comments_route_threads_and_all_flag() {
    let _guard = crate::ENV_SERIAL.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.rs"), "fn open() {}\nfn done() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.rs"), "fn open() {}\nfn done() {}\n// extra\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feat"]);

    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r", "feature", "main").await;

    let open = comment_on(&client, &base, id, "a.rs", 1, "open thread", "new").await;
    let open_id = open["id"].as_str().unwrap();
    let resp = client
        .post(format!("{base}/api/annotations"))
        .json(&serde_json::json!({
            "repo": "r", "path": "a.rs", "body": "a reply",
            "parent_id": open_id,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);

    let done = comment_on(&client, &base, id, "a.rs", 2, "resolved thread", "new").await;
    let done_id = done["id"].as_str().unwrap();
    let resp = client
        .patch(format!("{base}/api/annotations/{done_id}"))
        .json(&serde_json::json!({ "resolved": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);

    let listed = comments_at(&client, &base, id, "latest").await;
    let comments = listed["groups"][0]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["body"], "open thread");
    assert_eq!(comments[0]["replies"].as_array().unwrap().len(), 1);
    assert_eq!(comments[0]["replies"][0]["body"], "a reply");

    let resp = client
        .get(format!("{base}/api/reviews/{id}/comments"))
        .query(&[("all", "true")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let all: serde_json::Value = resp.json().await.unwrap();
    let comments = all["groups"][0]["comments"].as_array().unwrap();
    assert_eq!(comments.len(), 2);
}

/// GET comments accepts loopback and bearer; 401s a token-less
/// non-loopback request (XFF spoof — same technique as
/// `local_review_routes.rs` and `boot_e2e`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn comments_route_accepts_bearer_and_loopback_401s_nonloopback() {
    let _guard = crate::ENV_SERIAL.lock().await;
    const FIXTURE_TOKEN: &str = "kb-code-review-comments-test-token";
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c2"]);

    let (_daemon, base) = boot_with_repo("r", dir, ReviewSection::default()).await;
    std::env::remove_var("KB_CODE_TOKEN");

    let client = reqwest::Client::new();
    let id = create_review(&client, &base, "r", "feature", "main").await;

    // Loopback, no token — bypass.
    let resp = client
        .get(format!("{base}/api/reviews/{id}/comments"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "loopback must bypass auth_bearer"
    );

    // Simulated non-loopback, no token → 401.
    let resp = client
        .get(format!("{base}/api/reviews/{id}/comments"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "token-less non-loopback must 401"
    );

    // Same spoof + bearer → 200.
    let resp = client
        .get(format!("{base}/api/reviews/{id}/comments"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "bearer + non-loopback must be admitted"
    );
}
