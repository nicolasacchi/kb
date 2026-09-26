//! RS-U10a — the agent-facing review verbs against a real daemon: the
//! `--json` envelope on stdout, the typed error on stderr, the documented
//! exit codes, `pr:<N>` addressing, and `diff`/`log`/`cat` answering with
//! NO `refs/kbc/*` left in the clone. `[transcripts] enabled = false`.

use crate::common::git;
use assert_cmd::Command;
use kb_code_server::config::{
    GithubSection, KbCodeConfig, KbDaemonSection, RepoEntry, TranscriptsSection,
};
use std::path::{Path, PathBuf};

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A clone whose `origin` (github.com-shaped, `insteadOf`-rewritten to a
/// local bare repo; synthetic acme/widgets) carries `refs/pull/42/head`.
fn pr_fixture() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    let bare_tmp = tempfile::tempdir().unwrap();
    let bare = std::fs::canonicalize(bare_tmp.path())
        .unwrap()
        .join("origin.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    let repo_tmp = tempfile::tempdir().unwrap();
    let repo = std::fs::canonicalize(repo_tmp.path()).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("lib.rs"), "fn a() {}\n").unwrap();
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
    git(&repo, &["checkout", "-q", "-b", "pr"]);
    std::fs::write(repo.join("lib.rs"), "fn a() { /* x */ }\n").unwrap();
    std::fs::write(repo.join(".env"), "TOKEN=ghp_FAKEFAKEFAKE\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "pr: touch lib"]);
    git(
        &repo,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            "HEAD:refs/pull/42/head",
        ],
    );
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["branch", "-q", "-D", "pr"]);
    (repo_tmp, bare_tmp, repo)
}

async fn boot(repo: &Path) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: "widgets".to_string(),
            path: repo.to_path_buf(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        transcripts: TranscriptsSection {
            enabled: false,
            root: "/nonexistent".into(),
            exclude_projects: Vec::new(),
            index_thinking: false,
        },
        github: GithubSection {
            token_file: None,
            api_base: "http://127.0.0.1:1".to_string(),
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

/// Run `kb-code <args> --daemon <url>`; return (exit code, stdout, stderr).
fn run(url: &str, args: &[&str]) -> (i32, String, String) {
    let mut all: Vec<&str> = args.to_vec();
    all.extend(["--daemon", url]);
    let out = Command::cargo_bin("kb-code")
        .unwrap()
        .env_remove("KB_CODE_TOKEN")
        .args(all)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn envelope(stdout: &str, schema: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("stdout is one JSON doc ({e}): {stdout}"));
    assert_eq!(v["schema"], schema, "{v:#}");
    assert_eq!(v["ok"], true);
    assert!(v["data"].is_object());
    assert!(v["warnings"].is_array());
    assert!(v["degraded"].is_boolean());
    assert!(v["next"].is_array());
    v
}

fn typed_error(stderr: &str) -> serde_json::Value {
    let v: serde_json::Value = serde_json::from_str(stderr)
        .unwrap_or_else(|e| panic!("stderr is one JSON doc ({e}): {stderr}"));
    assert_eq!(v["ok"], false);
    assert!(v["error"]["code"]
        .as_str()
        .unwrap()
        .starts_with("urn:kb:errors:"));
    assert!(v["error"]["message"].is_string());
    assert!(v["error"]["next"].is_array());
    v
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn agent_review_verbs_contract() {
    let (_repo_tmp, _bare_tmp, repo) = pr_fixture();
    let (_d, url) = boot(&repo).await;

    // find before anything exists: an empty (not failing) query.
    let (code, out, _) = run(&url, &["review", "find", "--pr", "42", "--json"]);
    assert_eq!(code, 0);
    let v = envelope(&out, "kbc-review-find/1");
    assert_eq!(v["empty_reason"], "no-review-for-pr");

    // pr:42 before start-pr: typed not-found, exit 8, suggests start-pr.
    let (code, out, err) = run(&url, &["review", "diff", "pr:42", "--json"]);
    assert_eq!(code, 8, "{err}");
    assert!(out.is_empty(), "nothing on stdout for a failure");
    let e = typed_error(&err);
    assert_eq!(e["error"]["code"], "urn:kb:errors:review-not-found");

    // start-pr --json: the kbc-review-start/1 envelope.
    let (code, out, err) = run(
        &url,
        &[
            "review", "start-pr", "--repo", "widgets", "--pr", "42", "--json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    let v = envelope(&out, "kbc-review-start/1");
    let id = v["data"]["id"].as_i64().unwrap();
    assert_eq!(v["data"]["minted"], true);
    assert!(v["data"]["base"].is_object());
    assert!(v["data"]["base"]["merge_base"].is_string());
    // …and again: reused, nothing minted.
    let (_, out, _) = run(
        &url,
        &[
            "review", "start-pr", "--repo", "widgets", "--pr", "42", "--json",
        ],
    );
    let v = envelope(&out, "kbc-review-start/1");
    assert_eq!(v["data"]["id"], id);
    assert_eq!(v["data"]["minted"], false);

    // Strip every kb ref: the views must not need them.
    for r in git_out(&repo, &["for-each-ref", "--format=%(refname)", "refs/kbc"]).lines() {
        git(&repo, &["update-ref", "-d", r]);
    }

    // diff via pr:<N>, stat.
    let (code, out, err) = run(&url, &["review", "diff", "pr:42", "--json"]);
    assert_eq!(code, 0, "{err}");
    let v = envelope(&out, "kbc-review-diff/1");
    assert_eq!(v["data"]["review_id"], id);
    assert_eq!(v["data"]["pr"], 42);
    let paths: Vec<&str> = v["data"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"lib.rs") && paths.contains(&".env"),
        "{paths:?}"
    );

    // diff by id/ps, patch with a budget: truncated + marker + warning.
    let addr = format!("{id}/ps1");
    let (code, out, _) = run(&url, &["review", "diff", &addr, "--budget", "5", "--json"]);
    assert_eq!(code, 0);
    let v = envelope(&out, "kbc-review-diff/1");
    assert_eq!(v["data"]["mode"], "patch");
    assert_eq!(v["data"]["truncated"], true);
    assert!(!v["data"]["patch"].as_str().unwrap().contains("ghp_FAKE"));
    assert!(v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().starts_with("truncated:")));

    // human patch output is the raw diff text.
    let (code, out, _) = run(
        &url,
        &["review", "diff", &addr, "--patch", "--path", "lib.rs"],
    );
    assert_eq!(code, 0);
    assert!(out.contains("+fn a() { /* x */ }"), "{out}");

    // log + cat.
    let (code, out, _) = run(&url, &["review", "log", &id.to_string(), "--json"]);
    assert_eq!(code, 0);
    let v = envelope(&out, "kbc-review-log/1");
    assert_eq!(v["data"]["commits"][0]["subject"], "pr: touch lib");
    let (code, out, _) = run(
        &url,
        &["review", "cat", &id.to_string(), "lib.rs", "--side", "old"],
    );
    assert_eq!(code, 0);
    assert_eq!(out, "fn a() {}\n");
    let (code, _, err) = run(&url, &["review", "cat", &id.to_string(), ".env", "--json"]);
    assert_eq!(code, 4, "the secret denylist is a refusal: {err}");
    let e = typed_error(&err);
    assert_eq!(e["error"]["code"], "urn:kb:errors:redacted-by-policy");

    // Addressing failures.
    let (code, _, err) = run(&url, &["review", "diff", "pr:x", "--json"]);
    assert_eq!(code, 2);
    assert_eq!(typed_error(&err)["error"]["code"], "urn:kb:errors:usage");
    let (code, _, err) = run(
        &url,
        &[
            "review",
            "diff",
            &format!("{id}/ps1"),
            "--ps",
            "2",
            "--json",
        ],
    );
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = run(&url, &["review", "log", "9999", "--json"]);
    assert_eq!(code, 8);
    typed_error(&err);

    // verify before any compose: fails (no document, no verdict), exit 3.
    let (code, out, _) = run(&url, &["review", "verify", &id.to_string(), "--json"]);
    assert_eq!(code, 3);
    let v = envelope(&out, "kbc-review-verify/1");
    assert_eq!(v["data"]["result"], "fail");
    assert!(!v["next"].as_array().unwrap().is_empty());

    // compose --slugify: slug-less findings get kb's ASCII slugs; then
    // verify passes.
    let body = serde_json::json!({
        "summary": "Looks fine overall.",
        "verdict": "comment",
        "findings": {"schema": "kbc-findings/1", "findings": [{
            "severity": "concern",
            "category": "correctness",
            "location": {"path": "lib.rs", "kind": "whole_file"},
            "title": "Größe über alles",
            "rationale": "Non-ASCII title, no slug given.",
        }]},
    });
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("compose.json");
    std::fs::write(&file, serde_json::to_vec(&body).unwrap()).unwrap();
    let (code, _, err) = run(
        &url,
        &[
            "review",
            "compose",
            &id.to_string(),
            "--from-file",
            file.to_str().unwrap(),
            "--slugify",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("f-gr-e-ber-alles"), "{err}");
    let (code, out, err) = run(&url, &["review", "verify", &id.to_string(), "--json"]);
    assert_eq!(code, 0, "{out}{err}");
    let v = envelope(&out, "kbc-review-verify/1");
    assert_eq!(v["data"]["result"], "ok");
    assert_eq!(v["data"]["findings_count"], 1);

    // find now resolves.
    let (_, out, _) = run(&url, &["review", "find", "--pr", "42", "--json"]);
    let v = envelope(&out, "kbc-review-find/1");
    assert_eq!(v["data"]["preferred"]["widgets"], id);
}
