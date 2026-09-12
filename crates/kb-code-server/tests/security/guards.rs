//! SEC-02 — end-to-end coverage for the Origin/Host allowlist and the
//! `X-Kbc-Request: 1` mutation header, layered on the whole `/api` nest
//! (`router::build_router`).
//!
//! The contract these tests pin, in the order the middleware evaluates it:
//!
//! 1. **Admission for allowed callers is BYTE-IDENTICAL to pre-V70-A2.**
//!    A plain loopback request with no `Origin` gets the same status and
//!    the same body it always did — reads, mutations, loopback-only
//!    routes, and 404s alike. That is the first four tests, and it is the
//!    property the whole unit is judged on: a guard that changes the happy
//!    path is a regression wearing a security hat.
//! 2. A rebound `Host` is refused with the typed 403 — on EVERY tier,
//!    including the loopback-only sub-router (which otherwise 404s a
//!    non-loopback caller) and `/api/audit`.
//! 3. A foreign `Origin` is refused; a loopback origin, the request's own
//!    `Host`, and a configured `[server] hostnames` entry are not.
//! 4. A browser-originated mutation without `X-Kbc-Request` is refused —
//!    which is the guard that stops `http://localhost:3000` (an allowed
//!    ORIGIN, since it is loopback) from driving a mutation.
//! 5. `[security] strict_request_header = true` extends (4) to
//!    Origin-less callers.

use kb_code_server::config::{
    KbCodeConfig, KbDaemonSection, RepoEntry, SecuritySection, ServerSection,
};
use kb_core::paths::KbPaths;
use std::path::Path;

use crate::common::git;

fn fixture_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "c1"]);
    tmp
}

async fn boot(
    path: &Path,
    server: ServerSection,
    security: SecuritySection,
) -> (tempfile::TempDir, String) {
    let cfg = KbCodeConfig {
        server,
        security,
        repos: vec![RepoEntry {
            name: "fx".to_string(),
            path: std::fs::canonicalize(path).unwrap(),
        }],
        kb_daemon: KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        ..KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = KbPaths::rooted_at(tmp.path(), "kb-code");
    let (addr, _task) = kb_code_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    (tmp, format!("http://{addr}"))
}

async fn boot_default(path: &Path) -> (tempfile::TempDir, String) {
    boot(path, ServerSection::default(), SecuritySection::default()).await
}

/// The typed refusal shape every guard returns.
async fn assert_problem(resp: reqwest::Response, urn: &str) {
    assert_eq!(resp.status(), 403, "expected a typed 403");
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"].as_str(), Some(urn), "body: {body}");
    assert_eq!(body["status"].as_i64(), Some(403));
}

// --- 1. the happy path is unchanged --------------------------------------

#[tokio::test]
async fn a_plain_loopback_read_is_admitted_unchanged() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/repos"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["repos"][0]["name"].as_str(), Some("fx"));

    // ...and so is the unauthenticated liveness probe OUTSIDE the nest.
    let resp = client.get(format!("{base}/healthz")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn a_plain_loopback_mutation_needs_no_header_when_no_origin_is_sent() {
    // The CLI/curl/in-process-test posture: no `Origin`, no header, and
    // the mutation is admitted exactly as it was pre-V70-A2. `POST
    // /api/backfill` is an ordinary bearer-tier mutation with no body.
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/backfill?repo=fx"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "a header-less local POST must still pass"
    );
}

#[tokio::test]
async fn a_404_on_an_unknown_api_path_is_still_a_404() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn the_spa_fallback_carries_the_csp() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    // This daemon booted with no SPA dist, so the fallback is the
    // `spa-unavailable` 404 — which still carries the header set (the
    // point of applying them at ONE place).
    let resp = reqwest::Client::new()
        .get(format!("{base}/"))
        .send()
        .await
        .unwrap();
    let csp = resp
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert_eq!(csp, kb_code_server::spa::CSP);
    assert_eq!(
        resp.headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        resp.headers()
            .get("x-frame-options")
            .and_then(|v| v.to_str().ok()),
        Some("DENY")
    );
}

// --- 2. Host --------------------------------------------------------------

#[tokio::test]
async fn a_rebound_host_is_refused_on_every_tier() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    let client = reqwest::Client::new();

    for (method, path) in [
        // bearer read
        ("GET", "/api/repos"),
        // the audit read
        ("GET", "/api/audit"),
        // loopback-only tier — normally a 404 for a stranger, a 403 here:
        // the guard runs OUTSIDE `loopback_only`. That is not a route
        // oracle: an UNMATCHED `/api/...` path never reaches the nested
        // router's layers at all (axum falls through to the OUTER
        // fallback), so it 404s with or without a hostile Host — see
        // `a_404_on_an_unknown_api_path_is_still_a_404`.
        ("POST", "/api/checkout"),
    ] {
        let req = match method {
            "GET" => client.get(format!("{base}{path}")),
            _ => client.post(format!("{base}{path}")),
        };
        let resp = req.header("Host", "attacker.example").send().await.unwrap();
        assert_problem(resp, "urn:kb:errors:origin-refused").await;
    }
}

#[tokio::test]
async fn a_configured_hostname_is_admitted() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(
        repo.path(),
        ServerSection {
            hostnames: vec!["kbc.example".to_string()],
            ..Default::default()
        },
        SecuritySection::default(),
    )
    .await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/repos"))
        .header("Host", "kbc.example")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// --- 3. Origin ------------------------------------------------------------

#[tokio::test]
async fn a_foreign_origin_is_refused_and_a_loopback_one_is_not() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{base}/api/repos"))
        .header("Origin", "https://evil.example")
        .send()
        .await
        .unwrap();
    assert_problem(resp, "urn:kb:errors:origin-refused").await;

    // A loopback origin passes the ALLOWLIST — deliberately, so read
    // admission is byte-identical to today. What stops it mutating is the
    // header guard below, not this one.
    let resp = client
        .get(format!("{base}/api/repos"))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

// --- 4. the mutation header ----------------------------------------------

#[tokio::test]
async fn a_browser_mutation_without_the_header_is_refused() {
    // The residual SEC-02 case: another localhost page. Its Origin is
    // allowed (loopback), its peer is loopback (so every gate in the
    // router admits it) — and it still cannot mutate.
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/backfill?repo=fx"))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .unwrap();
    assert_problem(resp, "urn:kb:errors:missing-request-header").await;
}

#[tokio::test]
async fn a_browser_mutation_with_the_header_is_admitted() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/backfill?repo=fx"))
        .header("Origin", "http://localhost:3000")
        .header("X-Kbc-Request", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn a_browser_read_never_needs_the_header() {
    let repo = fixture_repo();
    let (_tmp, base) = boot_default(repo.path()).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/repos"))
        .header("Origin", "http://localhost:4747")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn strict_mode_extends_the_header_requirement_to_origin_less_callers() {
    let repo = fixture_repo();
    let (_tmp, base) = boot(
        repo.path(),
        ServerSection::default(),
        SecuritySection {
            strict_request_header: true,
            ..Default::default()
        },
    )
    .await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/api/backfill?repo=fx"))
        .send()
        .await
        .unwrap();
    assert_problem(resp, "urn:kb:errors:missing-request-header").await;

    let resp = client
        .post(format!("{base}/api/backfill?repo=fx"))
        .header("X-Kbc-Request", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn the_mutation_header_exemption_set_is_pinned() {
    // ONE exemption, and it is a reviewed decision (see
    // `security::origin::MUTATION_HEADER_EXEMPT_PREFIXES`' doc): kb's own
    // reader SPA drives `PUT /api/doc-lens/pin` cross-origin and does not
    // send this header. Pinned so a later route cannot join the set by
    // accident — the same contract `cors_layer_route_set_is_pinned` holds
    // for the ACAO-carrying set.
    assert_eq!(
        kb_code_server::security::origin::MUTATION_HEADER_EXEMPT_PREFIXES,
        &["/api/doc-lens/pin"]
    );
}
