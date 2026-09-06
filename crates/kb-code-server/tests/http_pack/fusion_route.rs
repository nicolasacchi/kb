//! V3.2-B2 — provenance fusion + review-risk.
//!
//! Fixture: scripted multi-author history + injected `commit_sessions` /
//! `session_signals` (no live kb daemon). Asserts dual-author rows,
//! `agent_share: null` without sessions, pain terms, risk renormalization
//! surface, and weight=pain / tainted gates.

use crate::common::git;
use kb_code_server::config::{
    BehavioralSection, KbCodeConfig, KbDaemonSection, RepoEntry, ReviewSection, SemanticSection,
};
use kb_code_server::store::{CommitSessionRow, Store};
use kb_core::paths::KbPaths;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "alice@example.com"]);
    git(dir, &["config", "user.name", "Alice"]);
}

fn write_file(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn commit_as(dir: &Path, email: &str, name: &str, message: &str, files: &[(&str, &str)]) -> String {
    git(dir, &["config", "user.email", email]);
    git(dir, &["config", "user.name", name]);
    for (rel, contents) in files {
        write_file(dir, rel, contents);
    }
    git(dir, &["add", "-A"]);
    let env_date = "2024-06-01T12:00:00 +0000";
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_DATE", env_date)
        .env("GIT_COMMITTER_DATE", env_date)
        .output()
        .expect("git commit");
    assert!(
        out.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sha = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&sha.stdout).trim().to_string()
}

fn rev_parse(dir: &Path, rev: &str) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", rev])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// History:
/// - c1 alice: alpha.rs + beta.rs
/// - c2 alice: alpha.rs (will map to session s1)
/// - c3 bob: gamma.rs
fn build_fixture() -> (tempfile::TempDir, std::path::PathBuf, Vec<String>) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir).unwrap();
    init_repo(&dir);

    let s1 = commit_as(
        &dir,
        "alice@example.com",
        "Alice",
        "c1",
        &[("alpha.rs", "fn a() {}\n"), ("beta.rs", "fn b() {}\n")],
    );
    let s2 = commit_as(
        &dir,
        "alice@example.com",
        "Alice",
        "c2",
        &[("alpha.rs", "fn a() { let x = 1; }\n")],
    );
    let s3 = commit_as(
        &dir,
        "bob@example.com",
        "Bob",
        "c3",
        &[("gamma.rs", "fn g() {}\n")],
    );
    (tmp, dir, vec![s1, s2, s3])
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    store_path: std::path::PathBuf,
}

async fn boot(repo_name: &str, repo_dir: &Path) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: "http://127.0.0.1:0".to_string(),
            token_file: None,
            public_url: None,
        },
        semantic: SemanticSection::default(),
        behavioral: BehavioralSection {
            enabled: true,
            window_days: 3650,
            max_commit_files: 30,
        },
        review: ReviewSection {
            patchset_capture: true,
            max_patchsets: 50,
            ..ReviewSection::default()
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let store_path = paths.state.join("index.db");
    let (addr, task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    Boot {
        tmp,
        base: format!("http://{addr}"),
        task,
        store_path,
    }
}

/// Readiness: file_count AND symbol_count — never file_count alone.
async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<serde_json::Value>().await {
                if let Some(entry) = body["repos"]
                    .as_array()
                    .and_then(|repos| repos.iter().find(|r| r["name"] == repo))
                {
                    let count = entry["file_count"].as_u64().unwrap_or(0) as usize;
                    let symbols = entry["symbol_count"].as_u64().unwrap_or(0);
                    if count >= expected_files && symbols > 0 {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "index timeout waiting for symbol_count"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn post_backfill(base: &str, repo: &str) -> serde_json::Value {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/behavioral/backfill"))
        .query(&[("repo", repo)])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "backfill status");
    resp.json().await.unwrap()
}

fn inject_session_mapping(store_path: &Path, repo_name: &str, sha: &str, session_id: &str) {
    // Open the daemon's live store path — tests that need dual-author must
    // inject BEFORE backfill so apply_commits dual-writes session rows.
    let store = Store::open(store_path).expect("open store");
    let repo_id = store
        .repo_id(repo_name)
        .expect("repo_id query")
        .expect("repo row must exist after boot");
    let row = CommitSessionRow {
        confidence: "exact".into(),
        via: "test-inject".into(),
        session_id: Some(session_id.into()),
        kb: Some("memory-session".into()),
        display_name: Some("test sess".into()),
        started_at: Some(1_700_000_000),
        resolved_at: 1_700_000_100,
    };
    store
        .upsert_commit_session(repo_id, sha, &row)
        .expect("upsert commit session");
}

fn inject_session_signals(store_path: &Path, repo_name: &str, session_id: &str, error_count: i64) {
    let store = Store::open(store_path).expect("open store");
    let repo_id = store
        .repo_id(repo_name)
        .expect("repo_id query")
        .expect("repo row must exist after boot");
    store
        .upsert_session_signals(repo_id, session_id, 0, error_count, 120, 1_700_000_200)
        .expect("upsert signals");
}

#[tokio::test]
async fn agent_share_null_without_session_data() {
    let (_tmp, repo_dir, _shas) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 3).await;
    post_backfill(&boot.base, "demo").await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/behavioral/ownership", boot.base))
        .query(&[("repo", "demo"), ("path", "alpha.rs")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["agents"].as_array().unwrap().is_empty());
    assert!(
        body["agent_share"].is_null(),
        "agent_share must be null not 0.0 when no sessions: {body}"
    );
    // Human authors still present.
    assert!(!body["authors"].as_array().unwrap().is_empty());
    assert_eq!(body["total_commits"].as_i64().unwrap(), 2);
}

#[tokio::test]
async fn dual_author_and_agents_when_session_injected() {
    let (_tmp, repo_dir, shas) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 3).await;

    // Inject session mapping for c1+c2 before backfill dual-writes.
    inject_session_mapping(&boot.store_path, "demo", &shas[0], "sess-alpha");
    inject_session_mapping(&boot.store_path, "demo", &shas[1], "sess-alpha");
    post_backfill(&boot.base, "demo").await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/behavioral/ownership", boot.base))
        .query(&[("repo", "demo"), ("path", "alpha.rs")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let authors = body["authors"].as_array().unwrap();
    assert!(
        authors.iter().any(|a| a["author"] == "alice@example.com"),
        "human author retained: {body}"
    );
    assert!(
        !authors
            .iter()
            .any(|a| a["author"].as_str().unwrap_or("").starts_with("session:")),
        "session rows must not appear in human authors: {body}"
    );
    let agents = body["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0]["session_id"], "sess-alpha");
    assert_eq!(agents[0]["commits"].as_i64().unwrap(), 2);
    let share = body["agent_share"].as_f64().unwrap();
    assert!((share - 1.0).abs() < 1e-9, "agent_share={share}");

    // Session-coupling from dual-author paths of sess-alpha (alpha + beta from c1).
    let resp = client
        .get(format!("{}/api/behavioral/session-coupling", boot.base))
        .query(&[("repo", "demo"), ("session", "sess-alpha")])
        .send()
        .await
        .unwrap();
    let body: serde_json::Value = resp.json().await.unwrap();
    let pairs = body["pairs"].as_array().unwrap();
    assert!(
        !pairs.is_empty() || body.get("unavailable_reason").is_some(),
        "expected pairs or unavailable_reason: {body}"
    );
    if !pairs.is_empty() {
        let has_ab = pairs.iter().any(|p| {
            (p["path_a"] == "alpha.rs" && p["path_b"] == "beta.rs")
                || (p["path_a"] == "beta.rs" && p["path_b"] == "alpha.rs")
        });
        assert!(has_ab, "expected alpha/beta pair: {body}");
    }
}

#[tokio::test]
async fn weight_pain_and_tainted_gate_without_signals() {
    let (_tmp, repo_dir, _) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 3).await;
    post_backfill(&boot.base, "demo").await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/behavioral/hotspots", boot.base))
        .query(&[("repo", "demo"), ("weight", "pain")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "weight=pain must 400 without signals");

    let resp = client
        .get(format!("{}/api/behavioral/tainted", boot.base))
        .query(&[("repo", "demo"), ("path", "alpha.rs")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400, "tainted must 400 without signals");
}

#[tokio::test]
async fn weight_pain_with_signals_extends_terms() {
    let (_tmp, repo_dir, shas) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 3).await;
    inject_session_mapping(&boot.store_path, "demo", &shas[0], "sess-pain");
    inject_session_mapping(&boot.store_path, "demo", &shas[1], "sess-pain");
    post_backfill(&boot.base, "demo").await;
    inject_session_signals(&boot.store_path, "demo", "sess-pain", 10);

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/behavioral/hotspots", boot.base))
        .query(&[("repo", "demo"), ("weight", "pain")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["weight"], "pain");
    let alpha = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["path"] == "alpha.rs")
        .expect("alpha hotspot");
    assert!(
        alpha["hotspot"]["terms"]["pain"].as_f64().is_some(),
        "pain term present: {alpha}"
    );
}

#[tokio::test]
async fn review_risk_terms_and_null_when_nothing() {
    let (_tmp, repo_dir, shas) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 3).await;
    inject_session_mapping(&boot.store_path, "demo", &shas[1], "sess-rev");
    post_backfill(&boot.base, "demo").await;
    inject_session_signals(&boot.store_path, "demo", "sess-rev", 5);

    // Branch for review: change alpha.rs
    let base = rev_parse(&repo_dir, "HEAD");
    git(&repo_dir, &["checkout", "-q", "-b", "feature"]);
    commit_as(
        &repo_dir,
        "alice@example.com",
        "Alice",
        "feature: touch alpha",
        &[("alpha.rs", "fn a() { let x = 99; }\n")],
    );

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/reviews", boot.base))
        .json(&serde_json::json!({
            "repo": "demo",
            "head_ref": "feature",
            "base_ref": "main",
            "title": "risk test",
            "session_id": "sess-rev",
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let created_text = resp.text().await.unwrap();
    assert_eq!(status, 201, "create review: {created_text}");
    let created: serde_json::Value = serde_json::from_str(&created_text).unwrap();
    let id = created["id"].as_i64().expect("review id");

    // Warm risk: counters only after backfill.
    let t0 = Instant::now();
    let resp = client
        .get(format!("{}/api/reviews/{id}/risk", boot.base))
        .send()
        .await
        .unwrap();
    let elapsed = t0.elapsed();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["schema"], "review-risk/1");
    assert!(body["note"].as_str().unwrap().contains("ATTENTION"));
    let files = body["files"].as_array().unwrap();
    assert!(!files.is_empty());
    let alpha = files
        .iter()
        .find(|f| f["path"] == "alpha.rs")
        .expect("alpha in risk");
    assert!(
        !alpha["risk"].is_null(),
        "risk should be computable: {alpha}"
    );
    assert!(alpha["risk"]["terms"]["relative_churn"].is_number());
    assert!(alpha["risk"]["score"].as_f64().is_some());
    // session_pain present because we injected signals + review session_id
    assert!(
        alpha["risk"]["terms"]["session_pain"].is_number()
            || alpha["inputs_missing"]
                .as_array()
                .map(|a| a.iter().any(|x| x == "session_pain"))
                .unwrap_or(false),
        "session_pain term or listed missing: {alpha}"
    );
    // Latency budget for small review (p50 < 250ms warm — single sample).
    assert!(
        elapsed < Duration::from_millis(250),
        "risk latency {elapsed:?} exceeds 250ms warm budget (base={base})"
    );
}

#[tokio::test]
async fn session_coupling_unavailable_reason_when_unknown_session() {
    let (_tmp, repo_dir, _) = build_fixture();
    let boot = boot("demo", &repo_dir).await;
    wait_for_indexed(&boot.base, "demo", 3).await;
    post_backfill(&boot.base, "demo").await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/behavioral/session-coupling", boot.base))
        .query(&[("repo", "demo"), ("session", "never-seen")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["pairs"].as_array().unwrap().is_empty());
    assert!(
        body["unavailable_reason"]
            .as_str()
            .unwrap()
            .contains("no dual-author"),
        "{body}"
    );
}

#[test]
fn library_dual_author_apply_and_risk_math() {
    // Pure store path — dual author + agent_share null/present + risk null.
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("i.db")).unwrap();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();

    store
        .apply_behavioral_commit(
            repo_id,
            &[("a.rs".into(), 1, 0)],
            "alice@ex.com",
            100,
            &[],
            Some("s1"),
        )
        .unwrap();
    let rows = store.author_stats_for(repo_id, "a.rs").unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r.author == "alice@ex.com"));
    assert!(rows.iter().any(|r| r.author == "session:s1"));

    // No signals → pain null path is caller's job; risk math unit-covered in mod tests.
    assert!(store.session_signals_for(repo_id, "s1").unwrap().is_none());
    store
        .upsert_session_signals(repo_id, "s1", 0, 10, 60, 200)
        .unwrap();
    let sig = store.session_signals_for(repo_id, "s1").unwrap().unwrap();
    // Stored fail_count is the not-measured sentinel (nothing populates it
    // today), so it must NOT dilute the error evidence: 10 errors =
    // error_norm 1.0 and the score is that alone, not (1+0)/2.
    let pain = kb_code_server::behavioral::pain_from_signals(
        sig.error_count,
        kb_code_server::behavioral::stored_fail_term(sig.fail_count),
    );
    assert!((pain.score - 1.0).abs() < 1e-9);
    assert!(pain.terms.fail_norm.is_none());
}
