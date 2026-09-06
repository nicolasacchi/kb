//! Daemon boot integration tests (W1.2): the imported invariant-#3/#4
//! predicates + invariant-#21 IPv6 companion actually wire up correctly
//! end-to-end (not just the pure-fn unit tests already covered by
//! `kb-server`'s own middleware suite, which this crate doesn't duplicate —
//! see the CLAUDE.md scope note: "kb-server's existing middleware tests
//! still green" is the pure-logic coverage; this file proves kb-code's
//! *wiring* of that imported logic).
//!
//! `KB_CODE_TOKEN` / `KB_ALLOW_NO_AUTH` are process env vars, so the tests
//! that mutate them serialize on `ENV_TEST_LOCK` — the same *purpose* as
//! `kb_core::config`'s own env-var tests' `ENV_TEST_LOCK`
//! (`crates/kb-core/src/config.rs`), but a `tokio::sync::Mutex`: the guard
//! here is held across `.await` (env mutation → async boot → async
//! requests → cleanup, all serialized), and `clippy::await_holding_lock`
//! correctly refuses that with a `std::sync::Mutex`.

use kb_code_server::config::KbCodeConfig;
use kb_core::paths::KbPaths;

static ENV_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn url(addr: std::net::SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

/// `KbPaths::rooted_at(tmpdir, "kb-code")` — every test boots its own
/// isolated store (W1.5 `bind_and_spawn` now opens `<state>/index.db` on
/// every boot), never the real XDG paths (would race under parallel
/// `cargo test`) — same rationale as `kb_server`'s own boot tests using
/// `serve_on_random_port_with_paths`.
fn isolated_paths(root: &std::path::Path) -> KbPaths {
    KbPaths::rooted_at(root, "kb-code")
}

#[tokio::test]
async fn boot_serves_identity_and_healthz() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = KbCodeConfig::default();
    let (addr, _task) =
        kb_code_server::serve_on_random_port_with_paths(cfg, isolated_paths(tmp.path()))
            .await
            .expect("serve_on_random_port_with_paths");

    let client = reqwest::Client::new();

    let resp = client
        .get(url(addr, "/healthz"))
        .send()
        .await
        .expect("GET /healthz");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"].as_str(), Some("ok"));

    // Loopback request, no Authorization header — bypasses auth_bearer
    // (invariant #4) since no KB_CODE_TOKEN is set in this test.
    let resp = client
        .get(url(addr, "/api/identity"))
        .send()
        .await
        .expect("GET /api/identity");
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"].as_str(), Some("kb-code"));
    assert!(body["version"].as_str().is_some());
    assert!(body["started_at"].as_str().is_some());
    assert_eq!(body["repos"].as_array().map(|a| a.len()), Some(0));
    // No `[kb_daemon] public_url` configured — `kb_public_url` falls back
    // to the federation `url`'s own default (`KbDaemonSection::DEFAULT_URL`).
    assert_eq!(
        body["kb_public_url"].as_str(),
        Some("http://127.0.0.1:4000")
    );
    // invariant:2 kb-sibling/1 Hello — kb-code mirrors kb's identity
    // fields so the contract is checkable in BOTH directions.
    assert_eq!(body["sibling_protocol"].as_str(), Some("kb-sibling/1"));
    assert_eq!(body["sibling_major"].as_u64(), Some(1));
    assert_eq!(
        body["schema_epoch"].as_u64(),
        Some(u64::from(kb_code_server::store::schema_epoch())),
    );
    // Unstamped local build ⇒ the honest "dev", never a fabricated sha.
    assert!(body["build_sha"].as_str().is_some_and(|s| !s.is_empty()));
    // S2-B — additive capability discovery, fail-closed default.
    assert_eq!(body["remote_mutations"].as_bool(), Some(false));
}

/// kb-code's `/healthz` stays PURE liveness too (same posture as kb's) —
/// the Hello lives on `/api/identity` alone.
#[tokio::test]
async fn healthz_carries_no_sibling_or_schema_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(
        KbCodeConfig::default(),
        isolated_paths(tmp.path()),
    )
    .await
    .expect("serve_on_random_port_with_paths");

    let body: serde_json::Value = reqwest::Client::new()
        .get(url(addr, "/healthz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["status"].as_str(), Some("ok"));
    for field in ["sibling_protocol", "sibling_major", "schema_epoch"] {
        assert!(
            body.get(field).is_none(),
            "/healthz must stay pure liveness — found {field}"
        );
    }
}

/// S2-B — `GET /api/identity`'s `remote_mutations` field mirrors
/// `[review] remote_mutations` exactly (additive capability discovery, see
/// `routes::IdentityResponse`'s own doc).
#[tokio::test]
async fn identity_reflects_remote_mutations_when_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = KbCodeConfig {
        review: kb_code_server::config::ReviewSection {
            remote_mutations: true,
            ..Default::default()
        },
        ..KbCodeConfig::default()
    };
    let (addr, _task) =
        kb_code_server::serve_on_random_port_with_paths(cfg, isolated_paths(tmp.path()))
            .await
            .expect("serve_on_random_port_with_paths");

    let body: serde_json::Value = reqwest::Client::new()
        .get(url(addr, "/api/identity"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["remote_mutations"].as_bool(), Some(true));
}

#[tokio::test]
async fn identity_reports_configured_kb_public_url() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = KbCodeConfig {
        kb_daemon: kb_code_server::config::KbDaemonSection {
            url: "http://kb:4000".to_string(),
            public_url: Some("https://kb.example.com".to_string()),
            ..kb_code_server::config::KbDaemonSection::default()
        },
        ..KbCodeConfig::default()
    };
    let (addr, _task) =
        kb_code_server::serve_on_random_port_with_paths(cfg, isolated_paths(tmp.path()))
            .await
            .expect("serve_on_random_port_with_paths");

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/identity"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // `public_url` set → `public_base()` prefers it over the container-
    // hostname `url`, which a browser could never resolve.
    assert_eq!(
        body["kb_public_url"].as_str(),
        Some("https://kb.example.com")
    );
}

#[tokio::test]
async fn identity_reports_configured_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"])
        .status()
        .expect("git runs")
        .success();
    assert!(ok, "git init failed");

    let cfg = KbCodeConfig {
        repos: vec![kb_code_server::config::RepoEntry {
            name: "myrepo".to_string(),
            path: std::fs::canonicalize(&repo).unwrap(),
        }],
        ..KbCodeConfig::default()
    };
    // Reuses `tmp` for the store root too — `KbPaths::rooted_at` nests
    // under `tmp/state/kb-code/`, `tmp/config/`, `tmp/cache/`, disjoint
    // from `tmp/repo` above.
    let (addr, _task) =
        kb_code_server::serve_on_random_port_with_paths(cfg, isolated_paths(tmp.path()))
            .await
            .expect("serve_on_random_port_with_paths");

    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(url(addr, "/api/identity"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repos = body["repos"].as_array().expect("repos array");
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["name"].as_str(), Some("myrepo"));
    assert_eq!(
        repos[0]["path"].as_str(),
        Some(std::fs::canonicalize(&repo).unwrap().to_str().unwrap())
    );
    // W1.5 — freshly registered, nothing has indexed it yet.
    assert_eq!(repos[0]["file_count"].as_u64(), Some(0));
    assert_eq!(repos[0]["symbol_count"].as_u64(), Some(0));
}

#[tokio::test]
async fn tokenless_nonloopback_bind_refuses_at_startup() {
    let _guard = ENV_TEST_LOCK.lock().await;
    std::env::remove_var("KB_CODE_TOKEN");
    std::env::remove_var("KB_ALLOW_NO_AUTH");
    let tmp = tempfile::tempdir().unwrap();

    // `0.0.0.0` is a real non-loopback `local_addr` once bound (mirrors
    // kb-server's own `daemon_refuses_public_bind_without_token` test) —
    // invariant #4's startup guard must refuse before spawning anything.
    let cfg = KbCodeConfig {
        server: kb_code_server::config::ServerSection {
            addr: "0.0.0.0:0".to_string(),
            ..Default::default()
        },
        ..KbCodeConfig::default()
    };
    let err = kb_code_server::bind_and_spawn(cfg, isolated_paths(tmp.path()))
        .await
        .expect_err("token-less public bind must refuse to start");
    let chain = format!("{err:#}");
    assert!(
        chain.contains("without a bearer token"),
        "expected the fail-closed startup-guard message; got: {chain}"
    );
}

#[tokio::test]
async fn allow_no_auth_override_permits_tokenless_public_bind() {
    let _guard = ENV_TEST_LOCK.lock().await;
    std::env::remove_var("KB_CODE_TOKEN");
    std::env::set_var("KB_ALLOW_NO_AUTH", "1");
    let tmp = tempfile::tempdir().unwrap();

    let cfg = KbCodeConfig {
        server: kb_code_server::config::ServerSection {
            addr: "0.0.0.0:0".to_string(),
            ..Default::default()
        },
        ..KbCodeConfig::default()
    };
    let result = kb_code_server::bind_and_spawn(cfg, isolated_paths(tmp.path())).await;

    std::env::remove_var("KB_ALLOW_NO_AUTH");

    let (_addr, task) = result.expect("KB_ALLOW_NO_AUTH=1 must permit the tokenless public bind");
    task.abort();
}

#[tokio::test]
async fn token_configured_enforces_on_nonloopback_and_loopback_bypasses() {
    const FIXTURE_TOKEN: &str = "kb-code-test-token-fixture";
    let _guard = ENV_TEST_LOCK.lock().await;
    std::env::remove_var("KB_ALLOW_NO_AUTH");
    std::env::set_var("KB_CODE_TOKEN", FIXTURE_TOKEN);
    let tmp = tempfile::tempdir().unwrap();

    let cfg = KbCodeConfig::default();
    let boot =
        kb_code_server::serve_on_random_port_with_paths(cfg, isolated_paths(tmp.path())).await;

    std::env::remove_var("KB_CODE_TOKEN");

    let (addr, _task) = boot.expect("serve_on_random_port_with_paths with KB_CODE_TOKEN set");
    let client = reqwest::Client::new();

    // Loopback peer (real TCP, no XFF) bypasses auth even with a token
    // configured (invariant #4).
    let resp = client.get(url(addr, "/api/identity")).send().await.unwrap();
    assert_eq!(resp.status(), 200, "loopback must bypass auth_bearer");

    // Simulated non-loopback client (peer=loopback is a trusted hop, so
    // X-Forwarded-For is consulted — same technique kb-server's own
    // auth_non_loopback_* tests use): no bearer → 401.
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // Same simulated non-loopback client, correct bearer → 200.
    let resp = client
        .get(url(addr, "/api/identity"))
        .header("X-Forwarded-For", "8.8.8.8")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // /healthz stays unauthenticated for the same simulated non-loopback
    // client (mounted outside the /api nest, mirroring kb-server).
    let resp = client
        .get(url(addr, "/healthz"))
        .header("X-Forwarded-For", "8.8.8.8")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}
