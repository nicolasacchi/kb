//! V76-R3c — file-at-ref, unknown-ref 404, frame claims, typeahead, compare.

use crate::common::{git, init_repo};
use kb_code_server::config::{KbCodeConfig, RepoEntry};
use kb_code_server::frames::ERR_UNKNOWN_REF;
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use tokio::sync::Mutex as AsyncMutex;

static SERIAL: AsyncMutex<()> = AsyncMutex::const_new(());

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

async fn boot_with_repos(
    repos: &[(&str, &Path)],
) -> (
    tempfile::TempDir,
    String,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let cfg = KbCodeConfig {
        repos: repos
            .iter()
            .map(|(name, path)| RepoEntry {
                name: name.to_string(),
                path: std::fs::canonicalize(path).unwrap(),
            })
            .collect(),
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve_on_random_port_with_paths");
    (tmp, format!("http://{addr}"), task)
}

/// Three commits on main, a branch, an annotated tag, a PR ref, HEAD~2.
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, String, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap();
    init_repo(&dir);
    std::fs::write(dir.join("a.txt"), b"c0\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c0"]);
    std::fs::write(dir.join("a.txt"), b"c1\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    std::fs::write(dir.join("a.txt"), b"c2\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "c2"]);
    git(&dir, &["branch", "feature/x"]);
    git(&dir, &["tag", "-a", "v1.0", "-m", "release"]);
    let head = git_out(&dir, &["rev-parse", "HEAD"]);
    git(&dir, &["update-ref", "refs/kbc/pr/7", &head]);
    git(&dir, &["update-ref", "refs/kbc/review/1/ps1", &head]);
    let head_2 = git_out(&dir, &["rev-parse", "HEAD~2"]);
    (tmp, dir, head, head_2)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_at_ref_reads_branch_tag_sha_headn_and_pr() {
    let _guard = SERIAL.lock().await;
    let (_keep, dir, head, head2) = fixture();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    for rev in ["feature/x", "v1.0", &head[..12], "HEAD~2", "refs/kbc/pr/7"] {
        let resp = client
            .get(format!("{base}/api/file"))
            .query(&[("repo", "fixture"), ("path", "a.txt"), ("ref", rev)])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200, "ref {rev}");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["frame"]["lane"], "file_at_ref", "ref {rev}");
        assert_eq!(body["frame"]["source"], "odb", "ref {rev}");
        assert_eq!(body["frame"]["class_ceiling"], "exact", "ref {rev}");
        assert!(body["frame"].get("refused").is_none(), "ref {rev}");
        let content = body["content"].as_str().unwrap();
        if rev == "HEAD~2" || rev == head2.as_str() {
            assert_eq!(content, "c0\n", "ref {rev}");
        } else {
            assert_eq!(content, "c2\n", "ref {rev}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_ref_is_working_tree_frame() {
    let _guard = SERIAL.lock().await;
    let (_keep, dir, _head, _head2) = fixture();
    std::fs::write(dir.join("a.txt"), b"dirty\n").unwrap();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/file"))
        .query(&[("repo", "fixture"), ("path", "a.txt")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["content"], "dirty\n");
    assert_eq!(body["frame"]["source"], "working_tree");
    assert_eq!(body["frame"]["class_ceiling"], "exact");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_ref_is_404_with_the_urn() {
    let _guard = SERIAL.lock().await;
    let (_keep, dir, _head, _head2) = fixture();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/file"))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.txt"),
            ("ref", "no-such-branch"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], ERR_UNKNOWN_REF);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dash_prefixed_ref_is_400_not_a_resolve() {
    let _guard = SERIAL.lock().await;
    let (_keep, dir, _head, _head2) = fixture();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/file"))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.txt"),
            ("ref", "--output=/tmp/x"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typeahead_ranks_exact_over_prefix_and_caps_with_true_totals() {
    let _guard = SERIAL.lock().await;
    let (_keep, dir, head, _head2) = fixture();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let client = reqwest::Client::new();

    let exact: serde_json::Value = client
        .get(format!("{base}/api/refs/typeahead"))
        .query(&[("repo", "fixture"), ("q", "main")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = exact["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.first().copied(), Some("main"), "got {names:?}");

    let pr: serde_json::Value = client
        .get(format!("{base}/api/refs/typeahead"))
        .query(&[("repo", "fixture"), ("q", "refs/kbc/pr/7")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pr["hits"][0]["kind"], "pr");
    assert_eq!(pr["hits"][0]["insert"], "refs/kbc/pr/7");

    let sha_q = &head[..8];
    let sha: serde_json::Value = client
        .get(format!("{base}/api/refs/typeahead"))
        .query(&[("repo", "fixture"), ("q", sha_q)])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        sha["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["sha"].as_str().is_some_and(|s| s.starts_with(sha_q))),
        "got {}",
        sha
    );

    let capped: serde_json::Value = client
        .get(format!("{base}/api/refs/typeahead"))
        .query(&[("repo", "fixture"), ("q", ""), ("limit", "1")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(capped["returned"], 1);
    assert!(capped["total"].as_u64().unwrap() > 1);
    assert_eq!(capped["truncated"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compare_file_returns_two_blobs_and_hunks() {
    let _guard = SERIAL.lock().await;
    let (_keep, dir, _head, _head2) = fixture();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/compare/file"))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.txt"),
            ("a", "HEAD~2"),
            ("b", "HEAD"),
        ])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["schema"], "kbc-compare-file/1");
    assert_eq!(body["a"]["content"], "c0\n");
    assert_eq!(body["b"]["content"], "c2\n");
    assert_eq!(body["a"]["frame"]["source"], "odb");
    let hunks = body["hunks"].as_array().expect("hunks");
    assert!(!hunks.is_empty(), "expected a hunk, got {body}");
    assert!(hunks[0]["header"].as_str().unwrap().starts_with("@@"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outline_at_ref_carries_the_file_at_ref_frame() {
    let _guard = SERIAL.lock().await;
    let (_keep, dir, _head, _head2) = fixture();
    let (_tmp, base, _task) = boot_with_repos(&[("fixture", &dir)]).await;
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/outline"))
        .query(&[("repo", "fixture"), ("path", "a.txt"), ("ref", "HEAD")])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["frame"]["lane"], "file_at_ref");
    assert_eq!(body["frame"]["source"], "odb");
}
