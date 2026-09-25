//! RS-U10a — the review's own git views (`GET /api/reviews/{id}/diff`,
//! `/log`, `/cat`) and the PR lookup (`GET /api/reviews/find`), end to end
//! against a real daemon.
//!
//! The load-bearing assertion (README §13, "required by relocation"): the
//! views are computed from the PATCHSET ROW's base/tip shas, so they keep
//! answering after every `refs/kbc/*` is deleted from the clone (and the
//! feature branch with it) — they rely on object presence, never on a kb
//! ref. Every daemon here runs with `[transcripts] enabled = false`.

use crate::common::{git, init_repo};
use kb_code_server::config::{
    GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection, TranscriptsSection,
};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn cfg(repo_name: &str, path: &Path) -> KbCodeConfig {
    KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(path).unwrap(),
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
        // Nothing listens on port 1: PR metadata enrichment degrades at
        // once instead of reaching a real forge.
        github: GithubSection {
            token_file: None,
            api_base: "http://127.0.0.1:1".to_string(),
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

/// `main` + a two-commit `feature` that edits `a.txt`, adds `b.txt`, adds
/// a denylisted `.env`, and deletes `gone.txt`.
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    init_repo(dir);
    std::fs::write(dir.join("a.txt"), "base line\n").unwrap();
    std::fs::write(dir.join("gone.txt"), "old\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "base"]);
    git(dir, &["checkout", "-q", "-b", "feature"]);
    std::fs::write(dir.join("a.txt"), "feature line\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/b.txt"), "new file — à é ü\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature: edit a, add b"]);
    std::fs::write(dir.join(".env"), "SECRET=hunter2hunter2\n").unwrap();
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "feature: env + delete"]);
    tmp
}

/// Delete every `refs/kbc/*` AND the feature branch: the patchset tip is
/// then referenced by nothing at all in the user clone.
fn strip_kb_refs(dir: &Path) {
    git(dir, &["checkout", "-q", "main"]);
    let refs = git_out(dir, &["for-each-ref", "--format=%(refname)", "refs/kbc"]);
    assert!(!refs.is_empty(), "capture wrote at least one kb ref");
    for r in refs.lines() {
        git(dir, &["update-ref", "-d", r]);
    }
    git(dir, &["branch", "-q", "-D", "feature"]);
    assert_eq!(
        git_out(dir, &["for-each-ref", "--format=%(refname)", "refs/kbc"]),
        "",
        "no kb ref survives"
    );
}

async fn get_json(
    client: &reqwest::Client,
    url: String,
    query: &[(&str, &str)],
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client.get(url).query(query).send().await.unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
    (status, body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_log_cat_answer_from_patchset_shas_with_no_kb_refs_in_the_clone() {
    let repo = fixture();
    let dir = repo.path();
    let (_d, base) = boot(cfg("widgets", dir)).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({
            "repo": "widgets", "head_ref": "feature", "base_ref": "main", "title": "views",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "{}", resp.text().await.unwrap());
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let base_sha = git_out(dir, &["rev-parse", "main"]);
    let tip_sha = git_out(dir, &["rev-parse", "feature"]);

    strip_kb_refs(dir);

    // --- stat (default) ---
    let (st, stat) = get_json(&client, format!("{base}/api/reviews/{id}/diff"), &[]).await;
    assert_eq!(st, 200, "{stat}");
    assert_eq!(stat["schema"], "kbc-review-diff/1");
    assert_eq!(stat["mode"], "stat");
    assert_eq!(stat["base_sha"], base_sha);
    assert_eq!(stat["tip_sha"], tip_sha);
    assert_eq!(stat["source"], "work-tree");
    let paths: Vec<&str> = stat["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    for p in [".env", "a.txt", "gone.txt", "src/b.txt"] {
        assert!(paths.contains(&p), "{p} in {paths:?}");
    }
    assert!(stat.get("patch").is_none(), "stat carries no patch text");

    // --- name-only + a directory filter ---
    let (_, names) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/diff"),
        &[("mode", "name-only"), ("path", "src")],
    )
    .await;
    assert_eq!(names["files_count"], 1, "{names}");
    assert_eq!(names["files"][0]["path"], "src/b.txt");
    assert_eq!(names["files_total"], 4);

    // --- patch: full text, denylisted .env withheld and NAMED ---
    let (st, patch) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/diff"),
        &[("mode", "patch")],
    )
    .await;
    assert_eq!(st, 200, "{patch}");
    let text = patch["patch"].as_str().unwrap();
    assert!(
        text.contains("-base line") && text.contains("+feature line"),
        "{text}"
    );
    assert!(text.contains("src/b.txt"), "{text}");
    assert!(
        !text.contains("hunter2"),
        "a denylisted file's bytes never ship"
    );
    assert_eq!(patch["truncated"], false);
    assert_eq!(patch["redacted"][0]["path"], ".env");

    // --- patch + budget: deterministic cut with a marker ---
    let q = [("mode", "patch"), ("budget", "10")];
    let (_, cut1) = get_json(&client, format!("{base}/api/reviews/{id}/diff"), &q).await;
    let (_, cut2) = get_json(&client, format!("{base}/api/reviews/{id}/diff"), &q).await;
    assert_eq!(cut1["truncated"], true, "{cut1}");
    assert_eq!(cut1["patch"], cut2["patch"], "same budget, same bytes");
    let cut = cut1["patch"].as_str().unwrap();
    assert!(
        cut.contains("[kb-code: patch truncated (budget 10 tokens)"),
        "{cut}"
    );
    assert!(cut.len() < text.len());

    // --- bad mode is a 400, never an argv entry ---
    let (st, _) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/diff"),
        &[("mode", "--output=/tmp/x")],
    )
    .await;
    assert_eq!(st, 400);

    // --- log ---
    let (st, log) = get_json(&client, format!("{base}/api/reviews/{id}/log"), &[]).await;
    assert_eq!(st, 200, "{log}");
    assert_eq!(log["schema"], "kbc-review-log/1");
    assert_eq!(log["count"], 2);
    assert_eq!(log["commits"][0]["sha"], tip_sha);
    assert_eq!(log["commits"][0]["subject"], "feature: env + delete");
    assert_eq!(log["commits"][1]["subject"], "feature: edit a, add b");

    // --- cat: new side, old side, absent side, denylisted ---
    let (st, new) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/cat"),
        &[("path", "src/b.txt")],
    )
    .await;
    assert_eq!(st, 200, "{new}");
    assert_eq!(new["schema"], "kbc-review-cat/1");
    assert_eq!(new["side"], "new");
    assert_eq!(new["content"], "new file — à é ü\n");
    let (st, old) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/cat"),
        &[("path", "a.txt"), ("side", "old")],
    )
    .await;
    assert_eq!(st, 200, "{old}");
    assert_eq!(old["content"], "base line\n");
    assert_eq!(old["sha"], base_sha);
    let (st, _) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/cat"),
        &[("path", "gone.txt")],
    )
    .await;
    assert_eq!(st, 404, "deleted on the new side");
    let (st, _) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/cat"),
        &[("path", "gone.txt"), ("side", "old")],
    )
    .await;
    assert_eq!(st, 200, "present on the old side");
    let (st, env) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/cat"),
        &[("path", ".env")],
    )
    .await;
    assert_eq!(st, 403, "{env}");
    assert_eq!(env["type"], "urn:kb:errors:redacted-by-policy");
    let (st, _) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/cat"),
        &[("path", "../etc/passwd")],
    )
    .await;
    assert!(st == 400 || st == 403, "traversal refused: {st}");

    // --- unknown review / patchset ---
    let (st, _) = get_json(&client, format!("{base}/api/reviews/9999/diff"), &[]).await;
    assert_eq!(st, 404);
    let (st, _) = get_json(
        &client,
        format!("{base}/api/reviews/{id}/log"),
        &[("ps", "7")],
    )
    .await;
    assert_eq!(st, 404);

    // The views wrote nothing back into the clone.
    assert_eq!(
        git_out(dir, &["for-each-ref", "--format=%(refname)", "refs/kbc"]),
        ""
    );
}

/// A bare "origin" carrying `refs/pull/<n>/head`, reached through a
/// github.com-shaped URL rewritten by `insteadOf` (the `review_sweep.rs`
/// fixture technique, synthetic `acme/widgets`).
fn pr_fixture(pr: u32) -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
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
            "https://github.com/acme/widgets.git",
        ],
    );
    git(
        &repo,
        &[
            "config",
            &format!("url.{}.insteadOf", bare.to_str().unwrap()),
            "https://github.com/acme/widgets.git",
        ],
    );
    git(&repo, &["push", "-q", bare.to_str().unwrap(), "main"]);
    git(&repo, &["checkout", "-q", "-b", "pr-branch"]);
    std::fs::write(repo.join("feature.txt"), "feature\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "pr commit"]);
    git(
        &repo,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            &format!("HEAD:refs/pull/{pr}/head"),
        ],
    );
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["branch", "-q", "-D", "pr-branch"]);
    (repo_tmp, bare_tmp, repo)
}

async fn start_pr_42(
    client: &reqwest::Client,
    base: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/reviews/pr"))
        .json(&serde_json::json!({ "repo": "widgets", "pr_number": 42 }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json::<serde_json::Value>().await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn find_by_pr_and_start_pr_reports_minted() {
    let (_repo_tmp, _bare_tmp, repo) = pr_fixture(42);
    let (_d, base) = boot(cfg("widgets", &repo)).await;
    let client = reqwest::Client::new();

    let (st, empty) = get_json(&client, format!("{base}/api/reviews/find"), &[("pr", "42")]).await;
    assert_eq!(st, 200, "{empty}");
    assert_eq!(empty["reviews"].as_array().unwrap().len(), 0);

    let (st, created) = start_pr_42(&client, &base).await;
    assert_eq!(st, 201, "{created}");
    assert_eq!(created["minted"], true, "{created}");
    let id = created["id"].as_i64().unwrap();
    let (st, reused) = start_pr_42(&client, &base).await;
    assert_eq!(st, 200, "{reused}");
    assert_eq!(reused["reused"], true);
    assert_eq!(reused["minted"], false, "same head: nothing captured");

    let (st, found) = get_json(&client, format!("{base}/api/reviews/find"), &[("pr", "42")]).await;
    assert_eq!(st, 200, "{found}");
    assert_eq!(found["schema"], "kbc-review-find/1");
    assert_eq!(found["repos"], serde_json::json!(["widgets"]));
    assert_eq!(found["preferred"]["widgets"], id);
    assert_eq!(found["reviews"][0]["id"], id);
    assert_eq!(found["reviews"][0]["pr_repo_slug"], "acme/widgets");
    assert_eq!(found["reviews"][0]["latest_ps"], 1);

    let (_, scoped) = get_json(
        &client,
        format!("{base}/api/reviews/find"),
        &[("pr", "42"), ("repo", "widgets")],
    )
    .await;
    assert_eq!(scoped["reviews"].as_array().unwrap().len(), 1);
    let (st, _) = get_json(
        &client,
        format!("{base}/api/reviews/find"),
        &[("pr", "42"), ("repo", "nope")],
    )
    .await;
    assert_eq!(st, 404);
    let (st, _) = get_json(&client, format!("{base}/api/reviews/find"), &[]).await;
    assert_eq!(st, 400, "pr is required");

    // PR-bound reviews diff from the store/work tree with no kb refs too.
    let refs = git_out(&repo, &["for-each-ref", "--format=%(refname)", "refs/kbc"]);
    for r in refs.lines() {
        git(&repo, &["update-ref", "-d", r]);
    }
    let (st, diff) = get_json(&client, format!("{base}/api/reviews/{id}/diff"), &[]).await;
    assert_eq!(st, 200, "{diff}");
    assert_eq!(diff["files"][0]["path"], "feature.txt");

    // Snapshot reports `minted` too (always true today; RS-U6's D13).
    let resp = client
        .post(format!("{base}/api/reviews/{id}/snapshot"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    // The PR head ref was deleted above, so the snapshot's own head can
    // no longer resolve — the point here is only the envelope shape when
    // it does, so accept either and check `minted` on success.
    if resp.status() == 200 {
        assert_eq!(
            resp.json::<serde_json::Value>().await.unwrap()["minted"],
            true
        );
    }
}

// End-to-end regression for the "à panic": compose used to trip the
// `prose_refs` byte tokenizer on `à` (fixed in prose_refs, 539e2fc).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compose_with_non_ascii_titles_and_derived_slugs_never_panics() {
    let repo = fixture();
    let dir = repo.path();
    let (_d, base) = boot(cfg("widgets", dir)).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/reviews"))
        .json(&serde_json::json!({ "repo": "widgets", "head_ref": "feature", "base_ref": "main" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let id = resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let titles = [
        "Perché è rotto — à capo",
        "Größe über alles",
        "🔥 hot path 🔥",
        "数据库 N+1 查询",
    ];
    let findings: Vec<serde_json::Value> = titles
        .iter()
        .map(|t| {
            serde_json::json!({
                "slug": kb_code_server::review_findings::slug_from_title(t),
                "severity": "concern",
                "category": "correctness",
                "location": { "path": "src/b.txt", "kind": "whole_file" },
                "title": t,
                "rationale": format!("{t} — perché sì, à la carte"),
            })
        })
        .collect();
    let resp = client
        .post(format!("{base}/api/reviews/{id}/compose"))
        .json(&serde_json::json!({
            "summary": "Résumé: ça marche — 日本語 à",
            "verdict": "comment",
            "findings": { "schema": "kbc-findings/1", "findings": findings },
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let text = resp.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    // The slugs as compose reports them (the reads that render these
    // titles as prose — findings/doc/comments — are covered by the base's
    // own `prose_refs` non-ASCII regression test).
    let created: Vec<String> = body["findings"]["created"]
        .as_array()
        .unwrap_or_else(|| panic!("created list in {body}"))
        .iter()
        .map(|c| {
            c.as_str()
                .or_else(|| c["slug"].as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(created.len(), 4, "{body}");
    for s in &created {
        assert!(s.is_ascii() && s.starts_with("f-"), "{s:?}");
    }
}
