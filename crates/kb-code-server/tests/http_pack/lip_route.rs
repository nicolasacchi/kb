//! PRR-L2 — end-to-end HTTP tests for the lip/1 live-LSP overlay:
//! `GET /api/resolve` (lsp-live TOP tier + blob-mismatch discard),
//! `GET /api/hover` (provider hover fills doc), `GET /api/usages`
//! (references merged into `exact`), `GET /api/diagnostics` (null-vs-empty),
//! and `GET /api/repos`'s `intel` status shape. Mirrors `resolve_route.rs`'s
//! own `boot`-style conventions (real daemon on a random port); the mock
//! lip server is a small local axum router — this test binary has no
//! access to `kb_code_server::join::kb_client::test_support::mock_kb_server`
//! (`pub(crate)` inside the library crate), so it spins its own, same shape.

use crate::common::{git, init_repo};
use axum::routing::{get, post};
use axum::{Json, Router};
use kb_code_server::config::{
    IntelProviderEntry, IntelSection, KbCodeConfig, KbDaemonSection, RepoEntry, SemanticSection,
};
use kb_core::paths::KbPaths;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::net::TcpListener;

fn commit(dir: &Path, file: &str, contents: &str, message: &str) {
    let path = dir.join(file);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
}

fn disabled_kb_daemon() -> KbDaemonSection {
    KbDaemonSection {
        enabled: false,
        url: "http://127.0.0.1:0".to_string(),
        token_file: None,
        public_url: None,
    }
}

/// Spins up a minimal mock lip/1 server on an ephemeral loopback port —
/// the SAME "mock server, own base URL seam" pattern `github.rs`'s
/// `api_base` config knob uses (`config::IntelProviderEntry::url` is this
/// module's equivalent seam).
async fn mock_lip_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

struct Boot {
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
    base: String,
    #[allow(dead_code)]
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn boot(repo_name: &str, repo_dir: &Path, intel: IntelSection) -> Boot {
    let cfg = KbCodeConfig {
        repos: vec![RepoEntry {
            name: repo_name.to_string(),
            path: std::fs::canonicalize(repo_dir).unwrap(),
        }],
        kb_daemon: disabled_kb_daemon(),
        semantic: SemanticSection::default(),
        intel,
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

async fn task_abort(boot: Boot) {
    boot.task.abort();
}

async fn wait_for_indexed(base: &str, repo: &str, expected_files: usize) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(resp) = client.get(format!("{base}/api/repos")).send().await {
            if let Ok(body) = resp.json::<Value>().await {
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
            "expected repo {repo:?} to report file_count >= {expected_files} within the deadline"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn provider(name: &str, url: String, repo: &str) -> IntelProviderEntry {
    IntelProviderEntry {
        name: name.to_string(),
        url,
        langs: vec!["rust".to_string()],
        repos: vec![repo.to_string()],
    }
}

// --- /api/resolve: lsp-live ranks first; blob-mismatch discards --------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_route_lsp_live_ranks_above_scip_and_file_local() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    let src = "fn widget() -> i32 {\n    1\n}\n\nfn main() {\n    widget();\n}\n";
    commit(dir, "a.rs", src, "c1");
    let blob_sha = git_blob_sha(src.as_bytes());

    let router = Router::new()
        .route(
            "/lip/identity",
            get(|| async { Json(json!({"protocol": "lip/1", "lip_major": 1})) }),
        )
        .route(
            "/lip/definition",
            post(move || {
                let sha = blob_sha.clone();
                async move {
                    Json(json!({
                        "verified_blob_sha": sha,
                        "results": [{"path": "a.rs", "line": 99, "col": 0, "end_line": 99, "end_col": 0}]
                    }))
                }
            }),
        );
    let (addr, _lip) = mock_lip_server(router).await;

    let intel = IntelSection {
        providers: vec![provider("rust-live", format!("http://{addr}"), "fixture")],
    };
    let boot = boot("fixture", dir, intel).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/resolve", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.rs"),
            ("line", "6"),
            ("col", "4"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let candidates = body["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty(), "body: {body}");
    assert_eq!(candidates[0]["precision"], "lsp-live", "body: {body}");
    assert_eq!(candidates[0]["class"], "exact");
    assert_eq!(
        candidates[0]["line"], 99,
        "must carry the lip-provided location, not the local one"
    );
    // The existing ladder's own file-local/locals hit must still be present
    // (never replaced, only ranked below the lsp-live hit).
    assert!(candidates.iter().any(|c| c["precision"] != "lsp-live"));

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resolve_route_falls_back_to_the_local_ladder_on_a_blob_mismatch() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    let src = "fn widget() -> i32 {\n    1\n}\n\nfn main() {\n    widget();\n}\n";
    commit(dir, "a.rs", src, "c1");

    let router = Router::new()
        .route(
            "/lip/identity",
            get(|| async { Json(json!({"protocol": "lip/1", "lip_major": 1})) }),
        )
        .route(
            "/lip/definition",
            post(|| async {
                Json(json!({
                    "verified_blob_sha": "not-the-real-hash",
                    "results": [{"path": "a.rs", "line": 99, "col": 0, "end_line": 99, "end_col": 0}]
                }))
            }),
        );
    let (addr, _lip) = mock_lip_server(router).await;

    let intel = IntelSection {
        providers: vec![provider("rust-live", format!("http://{addr}"), "fixture")],
    };
    let boot = boot("fixture", dir, intel).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/resolve", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.rs"),
            ("line", "6"),
            ("col", "4"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let candidates = body["candidates"].as_array().unwrap();
    assert!(!candidates.is_empty(), "body: {body}");
    assert_ne!(
        candidates[0]["precision"], "lsp-live",
        "a blob-mismatched lip answer must never surface — the local ladder must still answer, body: {body}"
    );
    assert!(candidates.iter().all(|c| c["precision"] != "lsp-live"));

    task_abort(boot).await;
}

// --- /api/hover: provider hover fills doc -------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hover_route_fills_doc_from_the_provider_when_blob_verified() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    let src = "fn widget() -> i32 {\n    1\n}\n\nfn main() {\n    widget();\n}\n";
    commit(dir, "a.rs", src, "c1");
    let blob_sha = git_blob_sha(src.as_bytes());

    let router = Router::new()
        .route(
            "/lip/identity",
            get(|| async { Json(json!({"protocol": "lip/1", "lip_major": 1})) }),
        )
        .route(
            "/lip/hover",
            post(move || {
                let sha = blob_sha.clone();
                async move {
                    Json(json!({
                        "verified_blob_sha": sha,
                        "results": [{"contents": "live hover text", "range": null}]
                    }))
                }
            }),
        );
    let (addr, _lip) = mock_lip_server(router).await;

    let intel = IntelSection {
        providers: vec![provider("rust-live", format!("http://{addr}"), "fixture")],
    };
    let boot = boot("fixture", dir, intel).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/hover", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.rs"),
            ("line", "6"),
            ("col", "4"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["precision"], "lsp-live", "body: {body}");
    assert_eq!(body["trust"], "exact");
    assert_eq!(body["symbol"]["doc"], "live hover text");

    task_abort(boot).await;
}

// --- /api/usages: references merged into exact --------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usages_route_merges_lip_references_into_the_exact_class() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    let src = "fn widget() -> i32 {\n    1\n}\n\nfn main() {\n    widget();\n}\n";
    commit(dir, "a.rs", src, "c1");
    let blob_sha = git_blob_sha(src.as_bytes());

    let router = Router::new()
        .route(
            "/lip/identity",
            get(|| async { Json(json!({"protocol": "lip/1", "lip_major": 1})) }),
        )
        .route(
            "/lip/references",
            post(move || {
                let sha = blob_sha.clone();
                async move {
                    Json(json!({
                        "verified_blob_sha": sha,
                        "results": [
                            {"path": "a.rs", "line": 6, "col": 4, "end_line": 6, "end_col": 10},
                            {"path": "a.rs", "line": 20, "col": 0, "end_line": 20, "end_col": 6},
                        ]
                    }))
                }
            }),
        );
    let (addr, _lip) = mock_lip_server(router).await;

    let intel = IntelSection {
        providers: vec![provider("rust-live", format!("http://{addr}"), "fixture")],
    };
    let boot = boot("fixture", dir, intel).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/usages", boot.base))
        .query(&[
            ("repo", "fixture"),
            ("path", "a.rs"),
            ("line", "6"),
            ("col", "4"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    let exact = body["exact"].as_array().unwrap();
    // The (line=6,col=4) location is ALREADY produced by the local ladder
    // (a real occurrence at the call site) — merging must not duplicate
    // it; the (line=20) location is genuinely new.
    assert!(
        exact.iter().any(|r| r["line"] == 20 && r["kind"] == "ref"),
        "body: {body}"
    );
    let count_at_call_site = exact.iter().filter(|r| r["line"] == 6).count();
    assert_eq!(
        count_at_call_site, 1,
        "must not duplicate an already-known exact row, body: {body}"
    );

    task_abort(boot).await;
}

// --- /api/diagnostics: null vs empty ------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostics_route_reports_null_with_no_provider_configured() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\n", "c1");

    let boot = boot("fixture", dir, IntelSection::default()).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/diagnostics", boot.base))
        .query(&[("repo", "fixture"), ("path", "a.rs")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(body["diagnostics"].is_null(), "body: {body}");
    assert_eq!(body["unavailable_reason"], "no_provider_configured");
    assert_eq!(body["fetched"], false);

    task_abort(boot).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diagnostics_route_distinguishes_an_empty_clean_result_from_null() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    let src = "fn a() {}\n";
    commit(dir, "a.rs", src, "c1");
    let blob_sha = git_blob_sha(src.as_bytes());

    let router = Router::new()
        .route(
            "/lip/identity",
            get(|| async {
                Json(json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["diagnostics"]}))
            }),
        )
        .route(
            "/lip/diagnostics",
            post(move || {
                let sha = blob_sha.clone();
                async move { Json(json!({"verified_blob_sha": sha, "results": []})) }
            }),
        );
    let (addr, _lip) = mock_lip_server(router).await;

    let intel = IntelSection {
        providers: vec![provider("rust-live", format!("http://{addr}"), "fixture")],
    };
    let boot = boot("fixture", dir, intel).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/diagnostics", boot.base))
        .query(&[("repo", "fixture"), ("path", "a.rs")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert!(body["diagnostics"].is_array(), "body: {body}");
    assert_eq!(body["diagnostics"].as_array().unwrap().len(), 0);
    assert_eq!(body["fetched"], true);
    assert!(body["unavailable_reason"].is_null());

    task_abort(boot).await;
}

// --- /api/repos: intel status shape --------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repos_route_reports_intel_status_null_when_unconfigured_and_a_shape_when_configured() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\n", "c1");

    // Boot #1: no provider configured at all.
    let boot1 = boot("fixture", dir, IntelSection::default()).await;
    wait_for_indexed(&boot1.base, "fixture", 1).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/repos", boot1.base))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let entry = body["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "fixture")
        .unwrap();
    assert!(entry["intel"].is_null(), "body: {body}");
    assert_eq!(
        entry["intel_providers"],
        json!([]),
        "zero providers configured must mean an empty intel_providers array, body: {body}"
    );
    task_abort(boot1).await;

    // Boot #2: a provider IS configured — the shape appears, `alive:
    // false` until a real resolve/hover/usages/diagnostics call happens
    // (see `LipClient::snapshot`'s doc — /api/repos never itself probes).
    let router = Router::new().route(
        "/lip/identity",
        get(|| async { Json(json!({"protocol": "lip/1", "lip_major": 1})) }),
    );
    let (addr, _lip) = mock_lip_server(router).await;
    let intel = IntelSection {
        providers: vec![provider("rust-live", format!("http://{addr}"), "fixture")],
    };
    let boot2 = boot("fixture", dir, intel).await;
    wait_for_indexed(&boot2.base, "fixture", 1).await;
    let resp2 = client
        .get(format!("{}/api/repos", boot2.base))
        .send()
        .await
        .unwrap();
    let body2: Value = resp2.json().await.unwrap();
    let entry2 = body2["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "fixture")
        .unwrap();
    assert_eq!(entry2["intel"]["provider"], "rust-live", "body: {body2}");
    assert_eq!(entry2["intel"]["langs"], json!(["rust"]));
    assert_eq!(
        entry2["intel"]["alive"], false,
        "never probed yet, must stay cheap"
    );
    assert_eq!(
        entry2["intel_providers"],
        json!([{"provider": "rust-live", "langs": ["rust"], "alive": false, "server_version": null}]),
        "body: {body2}"
    );
    task_abort(boot2).await;
}

/// S2-D — a mixed-language repo (e.g. kb itself: rust + typescript) can have
/// MORE than one `[[intel.providers]]` entry naming it, one per language.
/// `intel` (single-valued, back-compat) can only ever surface the FIRST
/// config-order match; `intel_providers` (additive) must surface ALL of
/// them, in config order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repos_route_intel_providers_lists_every_matching_provider_in_config_order() {
    let repo_tmp = tempfile::tempdir().unwrap();
    let dir = repo_tmp.path();
    init_repo(dir);
    commit(dir, "a.rs", "fn a() {}\n", "c1");

    let router = Router::new().route(
        "/lip/identity",
        get(|| async { Json(json!({"protocol": "lip/1", "lip_major": 1})) }),
    );
    let (rust_addr, _rust_lip) = mock_lip_server(router).await;
    let router2 = Router::new().route(
        "/lip/identity",
        get(|| async { Json(json!({"protocol": "lip/1", "lip_major": 1})) }),
    );
    let (ts_addr, _ts_lip) = mock_lip_server(router2).await;

    let intel = IntelSection {
        providers: vec![
            provider("rust-live", format!("http://{rust_addr}"), "fixture"),
            IntelProviderEntry {
                name: "ts-live".to_string(),
                url: format!("http://{ts_addr}"),
                langs: vec!["typescript".to_string()],
                repos: vec!["fixture".to_string()],
            },
        ],
    };
    let boot = boot("fixture", dir, intel).await;
    wait_for_indexed(&boot.base, "fixture", 1).await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/repos", boot.base))
        .send()
        .await
        .unwrap();
    let body: Value = resp.json().await.unwrap();
    let entry = body["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "fixture")
        .unwrap();

    // `intel` — the single-valued field — stays the FIRST config-order match.
    assert_eq!(entry["intel"]["provider"], "rust-live", "body: {body}");

    // `intel_providers` — the additive field — carries BOTH, in config
    // order.
    let providers = entry["intel_providers"].as_array().unwrap();
    assert_eq!(providers.len(), 2, "body: {body}");
    assert_eq!(providers[0]["provider"], "rust-live");
    assert_eq!(providers[0]["langs"], json!(["rust"]));
    assert_eq!(providers[1]["provider"], "ts-live");
    assert_eq!(providers[1]["langs"], json!(["typescript"]));

    task_abort(boot).await;
}

/// Git-compatible blob content hash — same formula `ingest::git_blob_hash`/
/// `kb-lip`'s `blob::git_blob_sha1` both use; duplicated here (this test
/// binary has no access to the library crate's private `ingest` module)
/// purely to compute the SAME value the daemon will compute server-side so
/// the mock lip server can echo it back as `verified_blob_sha`.
fn git_blob_sha(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(b"blob ");
    hasher.update(bytes.len().to_string());
    hasher.update(b"\0");
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}
