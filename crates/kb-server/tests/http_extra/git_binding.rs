//! GITX — `[kb.*] git_dir` / `git_prefix`: a corpus whose mount hides its
//! repo (a bind-mounted subdirectory) still gets git history, while ids and
//! stored paths stay exactly what they were without the keys.
//!
//! Layout per test: `tmp/repo` is the real repo (corpus at `docs/research`),
//! `tmp/corpus` is the "bind mount" (a plain copy, NO `.git` above it), and
//! `tmp/_git/x/.git` is the "read-only `.git` mount" outside every corpus.
use crate::common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const T_FIRST: i64 = 1_700_000_000;
const T_SECOND: i64 = 1_700_000_100;

struct Fixture {
    tmp: tempfile::TempDir,
    corpus: PathBuf,
    git_dir: PathBuf,
}

fn run_git(repo: &Path, date: i64, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.email=t@kb",
            "-c",
            "user.name=kb-test",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", format!("{date} +0000"))
        .env("GIT_COMMITTER_DATE", format!("{date} +0000"))
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git {args:?} failed");
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let research = repo.join("docs/research");
    std::fs::create_dir_all(&research).unwrap();
    run_git(&repo, T_FIRST, &["init", "-q"]);
    let file = research.join("doc.html");
    std::fs::write(
        &file,
        "<html><body><h1>Doc</h1><p>First text.</p></body></html>",
    )
    .unwrap();
    run_git(&repo, T_FIRST, &["add", "docs/research/doc.html"]);
    run_git(&repo, T_FIRST, &["commit", "-q", "-m", "first"]);
    std::fs::write(
        &file,
        "<html><body><h1>Doc</h1><p>Second text.</p></body></html>",
    )
    .unwrap();
    run_git(&repo, T_SECOND, &["add", "docs/research/doc.html"]);
    run_git(&repo, T_SECOND, &["commit", "-q", "-m", "second"]);

    // The "bind mount": only the corpus subdirectory, no `.git` above it.
    let corpus = tmp.path().join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::copy(&file, corpus.join("doc.html")).unwrap();
    // The "read-only .git mount", outside every corpus path.
    let git_dir = tmp.path().join("_git/x");
    std::fs::create_dir_all(&git_dir).unwrap();
    let ok = std::process::Command::new("cp")
        .arg("-r")
        .arg(repo.join(".git"))
        .arg(git_dir.join(".git"))
        .status()
        .unwrap()
        .success();
    assert!(ok, "cp .git");
    Fixture {
        tmp,
        corpus,
        git_dir,
    }
}

async fn boot(
    fx: &Fixture,
    git_dir: Option<PathBuf>,
    git_prefix: Option<&str>,
) -> std::net::SocketAddr {
    let daemon_name = format!(
        "test-gitx-{}",
        fx.tmp.path().file_name().unwrap().to_string_lossy()
    );
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(
        KbName::new("smoke").unwrap(),
        KbSection {
            path: fx.corpus.clone(),
            skip_patterns: Vec::new(),
            ui: UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: None,
            project_slugs: Vec::new(),
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: Some("git".into()),
            git_dir,
            git_prefix: git_prefix.map(str::to_string),
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            reconcile_secs: None,
            capture_dir: None,
            resurface: None,
            slo: None,
            id_patterns: Vec::new(),
        },
    );
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        server: ServerSection::default(),
        ui: UiSection::default(),
        indexer: Default::default(),
        storage: Default::default(),
        share: Default::default(),
        webhooks: None,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        retention: Default::default(),
        backup: Default::default(),
        identity: Default::default(),
        memory: Default::default(),
        kb: kb_map,
        projects: Default::default(),
        sessions: Default::default(),
    };
    let paths = KbPaths::rooted_at(fx.tmp.path(), daemon_name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    common::wait_docs_listed(addr, "smoke", 1).await;
    addr
}

fn doc_id() -> String {
    kb_core::ids::ArtifactId::from_path("doc.html")
        .as_str()
        .to_string()
}

async fn versions(addr: std::net::SocketAddr, query: &str) -> Value {
    let id = doc_id();
    let r = reqwest::get(format!(
        "http://{addr}/api/kb/smoke/artifacts/{id}/versions{query}"
    ))
    .await
    .unwrap();
    assert_eq!(r.status(), 200, "the id derived from `doc.html` resolves");
    r.json().await.unwrap()
}

/// `(id, path relative to the corpus root)` for every listed doc, sorted.
async fn listing(addr: std::net::SocketAddr, fx: &Fixture) -> Vec<(String, String)> {
    let docs: Vec<Value> = reqwest::get(format!("http://{addr}/api/kb/smoke/docs?limit=200"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let root = std::fs::canonicalize(&fx.corpus).unwrap();
    let mut out: Vec<(String, String)> = docs
        .iter()
        .map(|d| {
            let p = std::path::PathBuf::from(d["path"].as_str().unwrap());
            let rel = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
            (d["id"].as_str().unwrap().to_string(), rel)
        })
        .collect();
    out.sort();
    out
}

fn git_rows(v: &Value) -> Vec<&Value> {
    v["versions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["source"] == "git")
        .collect()
}

#[tokio::test]
async fn unbound_corpus_has_no_git_versions() {
    let fx = fixture();

    // Without the keys: the corpus mount has no `.git` above it -> no git rows.
    let plain = boot(&fx, None, None).await;
    let before = versions(plain, "").await;
    let unbound = listing(plain, &fx).await;
    assert_eq!(unbound, vec![(doc_id(), "doc.html".to_string())]);
    assert!(
        git_rows(&before).is_empty(),
        "no binding, no .git above the corpus: {before}"
    );
}

#[tokio::test]
async fn binding_yields_git_rows_with_unchanged_ids() {
    let fx = fixture();
    let addr = boot(&fx, Some(fx.git_dir.clone()), Some("docs/research")).await;
    // The id is the one derived from the corpus-relative path `doc.html`
    // (not `docs/research/doc.html`): `versions()` asserts a 200 on it.
    let v = versions(addr, "").await;
    let git = git_rows(&v);
    assert_eq!(git.len(), 2, "two commits via prefix + rel path: {v}");
    assert_eq!(git[0]["label"], "second", "newest first");
    assert_eq!(git[1]["label"], "first");

    // Ids and stored paths are exactly what an UNBOUND boot of the same
    // fixture yields (the binding only changes where git looks).
    let bound = listing(addr, &fx).await;
    assert_eq!(bound, vec![(doc_id(), "doc.html".to_string())]);
    let fx_plain = fixture();
    let plain = boot(&fx_plain, None, None).await;
    assert_eq!(
        bound,
        listing(plain, &fx_plain).await,
        "ids + paths unchanged"
    );
}

#[tokio::test]
async fn memento_at_resolves_a_commit_through_the_binding() {
    let fx = fixture();
    let addr = boot(&fx, Some(fx.git_dir.clone()), Some("docs/research")).await;
    let mid: Value = versions(addr, &format!("?at={}", T_FIRST + 50)).await;
    assert_eq!(mid["memento"]["found"], true, "{mid}");
    assert_eq!(mid["memento"]["version"]["source"], "git", "{mid}");
    assert_eq!(mid["memento"]["version"]["label"], "first", "{mid}");
    let exact: Value = versions(addr, &format!("?at={T_SECOND}")).await;
    assert_eq!(exact["memento"]["version"]["label"], "second", "{exact}");
    assert_eq!(exact["memento"]["exact"], true, "{exact}");
}

#[tokio::test]
async fn wrong_prefix_or_missing_dot_git_degrades_to_no_git() {
    let fx = fixture();
    // A prefix that does not match the repo layout finds no commits (honest
    // empty), never another file's history.
    let addr = boot(&fx, Some(fx.git_dir.clone()), Some("docs/other")).await;
    assert!(git_rows(&versions(addr, "").await).is_empty());

    // A git_dir with no `.git` is refused at boot-binding time: no git, and
    // NOT a silent fallback to an ancestor walk.
    let fx2 = fixture();
    let empty = fx2.tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let addr = boot(&fx2, Some(empty), Some("docs/research")).await;
    assert!(git_rows(&versions(addr, "").await).is_empty());
}
