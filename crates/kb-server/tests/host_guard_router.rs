//! SEC-02 — the `Host` guard as it is LAYERED IN THE REAL ROUTER.
//!
//! WHY a separate file instead of more cases in `middleware.rs`'s unit
//! tests: those drive a hand-built five-line `guarded_api()` whose comment
//! only RESTATES the production layer order ("Same order as build_router").
//! A comment is not a test. Two regressions stayed green under that
//! harness, and both are invisible to a miniature:
//!
//!   1. deleting the `.route_layer(from_fn_with_state(..., host_guard))` on
//!      `capture_share` in `router.rs` — `POST /capture` is a corpus WRITE
//!      mounted OUTSIDE the `/api` nest, so nothing else guards it;
//!   2. moving the `/api` nest's guard UNDER `auth_bearer` — auth has
//!      already granted operator authority to the rebound request by then,
//!      so the guard is moot on the very requests it exists for.
//!
//! This file boots the REAL `build_router` (via the crate's standard
//! in-process `serve_on_random_port_with_paths` harness, the same one
//! `end_to_end.rs` uses) and speaks raw HTTP/1.1 over a TCP socket so the
//! `Host` header is whatever the attacker chose — `reqwest` derives `Host`
//! from the URL, which is precisely the value under attack. The `raw_get`
//! idiom is copied from `end_to_end.rs::raw_get`.
//!
//! Every request here originates from a LOOPBACK peer (the test harness
//! connects to 127.0.0.1), which is precisely the rebinding victim
//! `host_gate_applies` describes: the gate applies unconditionally for a
//! loopback peer, so nothing has to be configured for these to bite.

mod common;

use kb_core::config::{
    DaemonSection, DefaultsSection, KbConfig, KbSection, ServerSection, UiSection,
};
use kb_core::paths::KbPaths;
use kb_core::types::KbName;
use std::collections::BTreeMap;

/// A token written into the fixture's token file. Used only to make
/// `auth_bearer` NOT admit a forged-XFF request on its own, so a request
/// that reaches the guard proves the guard ran FIRST (see
/// `guard_precedes_auth_on_api`).
const FIXTURE_TOKEN: &str = "test-token-fixture-deadbeef";

fn kb_section(path: std::path::PathBuf) -> KbSection {
    KbSection {
        path,
        skip_patterns: Vec::new(),
        ui: UiSection::default(),
        embedding_model: None,
        reranker_model: None,
        chunked_embeddings: false,
        graph_boost: None,
        outbound: None,
        atlas: None,
        templates: std::collections::BTreeMap::new(),
        memory_scope: None,
        project_slugs: Vec::new(),
        default_search_category: None,
        code_url: None,
        decay_policy: None,
        versions: None,
        reading_progress: None,
        search: Default::default(),
        indexable_extensions: None,
        reconcile_secs: None,
        capture_dir: None,
        resurface: None,
        slo: None,
        id_patterns: Vec::new(),
    }
}

/// A one-kb corpus in a tempdir. Boot serves the real router on a random
/// port; the returned TempDir must be held alive by the caller for the
/// daemon's lifetime.
async fn boot(with_token: bool) -> (tempfile::TempDir, std::net::SocketAddr) {
    boot_with(with_token, ServerSection::default(), None).await
}

async fn boot_with(
    with_token: bool,
    server: ServerSection,
    spa_dist: Option<std::path::PathBuf>,
) -> (tempfile::TempDir, std::net::SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("corpus");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("seed.md"), "# seed\n\none doc\n").unwrap();

    let daemon_name = format!("test-{}", tmp.path().file_name().unwrap().to_string_lossy());
    let mut kb_map: BTreeMap<KbName, KbSection> = BTreeMap::new();
    kb_map.insert(KbName::new("smoke").unwrap(), kb_section(source));
    let cfg = KbConfig {
        daemon: DaemonSection {
            name: Some(daemon_name.clone()),
        },
        server,
        ui: UiSection::default(),
        kb: kb_map,
        defaults: DefaultsSection {
            embedding_model: None,
            disable_embedder_fallback: true,
        },
        ..Default::default()
    };

    let paths = KbPaths::rooted_at(tmp.path(), daemon_name);
    if with_token {
        std::fs::create_dir_all(&paths.config).unwrap();
        std::fs::write(paths.token_file(), FIXTURE_TOKEN).unwrap();
    }
    let (addr, _task) = kb_server::serve_on_random_port_with_paths_and_spa(cfg, paths, spa_dist)
        .await
        .expect("serve");
    common::wait_http_up(addr).await;
    (tmp, addr)
}

/// One raw HTTP/1.1 exchange with an ATTACKER-CHOSEN `Host` and,
/// optionally, a forged `X-Forwarded-For`. Returns (status, raw response).
///
/// Raw sockets rather than `reqwest` because `reqwest` overwrites `Host`
/// with the URL authority — the one header the rebinding attacker does
/// NOT control is the one under test here.
async fn raw(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    host: &str,
    xff: Option<&str>,
) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
    if let Some(xff) = xff {
        req.push_str(&format!("X-Forwarded-For: {xff}\r\n"));
    }
    req.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf).into_owned();
    let status: u16 = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, resp)
}

fn assert_host_refused(status: u16, resp: &str, what: &str) {
    assert_eq!(
        status, 403,
        "{what} must be REFUSED by the guard, got: {resp}"
    );
    assert!(
        resp.contains(kb_server::middleware::ERR_HOST_REFUSED),
        "{what} must carry the host-refused urn (so the operator can \
         self-diagnose from `[server] hostnames`), got: {resp}"
    );
}

/// PIN (a) — a rebound `Host` is refused on `/api` through the REAL
/// router. Regression caught: dropping the `host_guard` layer from the
/// `/api` nest in `router.rs` (the miniature in `middleware.rs` keeps
/// passing because it has its own copy of the layer).
#[tokio::test]
async fn rebound_host_refused_on_api_through_real_router() {
    let (_tmp, addr) = boot(false).await;
    for path in ["/api/identity", "/api/kbs", "/api/kb/smoke/docs"] {
        let (status, resp) = raw(addr, "GET", path, "attacker.example:4000", None).await;
        assert_host_refused(status, &resp, &format!("GET {path} on a rebound Host"));
    }
}

/// PIN (a′) — the guard on `/api` decides BEFORE `auth_bearer`, not
/// after. The forged `X-Forwarded-For` makes the peer look non-loopback
/// to `auth_bearer`, which (with a token configured) would answer 401 on
/// its own. A 403 host-refused body therefore proves the guard ran first;
/// a 401 would mean the `/api` nest's guard had been moved UNDER
/// `auth_bearer`, where it decides only after authority has been granted —
/// the exact layering regression SEC-02's ordering comment forbids.
///
/// Also pins that a forged XFF cannot switch the guard OFF (the peer is
/// a real loopback peer, so `host_gate_applies` is true regardless): a
/// pass-through here would be a full rebinding bypass.
#[tokio::test]
async fn guard_precedes_auth_on_api_through_real_router() {
    let (_tmp, addr) = boot(true).await;
    let (status, resp) = raw(
        addr,
        "GET",
        "/api/identity",
        "attacker.example:4000",
        Some("8.8.8.8"),
    )
    .await;
    assert_host_refused(
        status,
        &resp,
        "GET /api/identity, rebound Host + forged XFF (must be the GUARD's 403, not auth's 401)",
    );
}

/// PIN (b) — a rebound `Host` is refused on `POST /capture` through the
/// REAL router. Regression caught: deleting the `.route_layer(...,
/// host_guard)` on `capture_share` in `router.rs`. `/capture` sits
/// OUTSIDE the `/api` nest by design (the Web Share Target action), so
/// with that `route_layer` gone the only thing in front of a corpus WRITE
/// is `auth_bearer` — which hands operator authority to any loopback
/// peer, i.e. to the rebound page. This is the assertion that fails when
/// it is deleted: the request reaches the handler instead of 403ing.
#[tokio::test]
async fn rebound_host_refused_on_capture_through_real_router() {
    let (_tmp, addr) = boot(false).await;
    let (status, resp) = raw(addr, "POST", "/capture", "attacker.example:4000", None).await;
    assert_host_refused(
        status,
        &resp,
        "POST /capture on a rebound Host (a corpus WRITE must never be \
         reachable from a rebound page)",
    );
}

/// PIN (b′) — same route, with a token configured and a forged XFF, so a
/// 401 from `auth_bearer` would be the tell that the guard on
/// `capture_share` is layered UNDER auth (or absent and auth is the only
/// thing left). 403 host-refused pins guard-before-auth on this route too.
#[tokio::test]
async fn guard_precedes_auth_on_capture_through_real_router() {
    let (_tmp, addr) = boot(true).await;
    let (status, resp) = raw(
        addr,
        "POST",
        "/capture",
        "attacker.example:4000",
        Some("8.8.8.8"),
    )
    .await;
    assert_host_refused(
        status,
        &resp,
        "POST /capture, rebound Host + forged XFF (must be the GUARD's 403, not auth's 401)",
    );
}

/// PIN (c) — a legitimate loopback request still succeeds through the
/// REAL router. Guards the other direction: the guard is not a blanket
/// 403 (a daemon that refuses `Host: localhost` is a daemon nobody can
/// use). Runs with the SAME token fixture as the guard-precedes tests,
/// proving the guard refuses only the rebound name and not loopback.
#[tokio::test]
async fn loopback_host_still_works_through_real_router() {
    let (_tmp, addr) = boot(true).await;
    let (status, resp) = raw(
        addr,
        "GET",
        "/api/identity",
        &format!("127.0.0.1:{}", addr.port()),
        None,
    )
    .await;
    assert_eq!(status, 200, "loopback Host must still be served: {resp}");

    let (status, resp) = raw(addr, "GET", "/api/kbs", "localhost", None).await;
    assert_eq!(status, 200, "Host: localhost must still be served: {resp}");
}

/// PIN (c′) — the share-target WRITE still works for a loopback caller
/// through the REAL router, so the `capture_share` `route_layer` pair is
/// load-bearing in both directions: (b) refuses the rebound name, this
/// admits the real one. A guard that refused everything would pass (b)
/// and fail here.
#[tokio::test]
async fn loopback_capture_share_target_still_works_through_real_router() {
    let (tmp, addr) = boot(false).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let form = reqwest::multipart::Form::new()
        .text("title", "Shared Note")
        .part(
            "files",
            reqwest::multipart::Part::text("# Shared Note\n\nbody text\n").file_name("shared.md"),
        );
    let resp = client
        .post(format!("http://{addr}/capture"))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status().as_u16(),
        303,
        "loopback share-target capture must still succeed: {:?}",
        resp.text().await
    );
    assert!(
        tmp.path().join("corpus/capture").exists(),
        "the file must actually have landed in the corpus"
    );
}

/// A1-4 / A3-6 / A13-7. `GET /metrics` is a top-level mount carrying
/// `auth_bearer`'s loopback bypass; it was missing the Host guard that
/// `GET /api/metrics` has, so a rebound page could read it (request
/// counts, embedder health, per-kb labels). Fails without the
/// `host_guard` route_layer in `prometheus_router`.
#[tokio::test]
async fn rebound_host_refused_on_prometheus_metrics_through_real_router() {
    let (_tmp, addr) = boot(false).await;
    let (status, resp) = raw(addr, "GET", "/metrics", "attacker.example:4000", None).await;
    assert_host_refused(status, &resp, "GET /metrics on a rebound Host");
    // ...and the guard sits before auth (forged XFF + token configured).
    let (_tmp2, addr2) = boot(true).await;
    let (status, resp) = raw(
        addr2,
        "GET",
        "/metrics",
        "attacker.example:4000",
        Some("8.8.8.8"),
    )
    .await;
    assert_host_refused(status, &resp, "GET /metrics, rebound Host + forged XFF");
    // a legitimate loopback scrape still works
    let (status, resp) = raw(addr, "GET", "/metrics", "localhost", None).await;
    assert_eq!(
        status, 200,
        "loopback /metrics must still be served: {resp}"
    );
}

/// A1.f5 route inventory. Every top-level (outside-the-`/api`-nest) route
/// that is not on the explicit exempt list must refuse a rebound Host.
/// A NEW top-level mount that forgets the guard fails here only if it is
/// added to `GUARDED_TOP_LEVEL` - which is the point: the list is the
/// reviewable inventory, and the exempt list below is the only place a
/// route may opt out (liveness + the SPA/artifact fallback, whose
/// permalink shell is guarded inside the dispatcher instead).
const GUARDED_TOP_LEVEL: &[(&str, &str)] = &[
    ("GET", "/api/identity"),
    ("POST", "/capture"),
    ("GET", "/metrics"),
];
const EXEMPT_TOP_LEVEL: &[&str] = &["/healthz", "/", "/api/"];

#[tokio::test]
async fn route_inventory_every_guarded_mount_refuses_a_rebound_host() {
    let (_tmp, addr) = boot(false).await;
    for (method, path) in GUARDED_TOP_LEVEL {
        let (status, resp) = raw(addr, method, path, "attacker.example:4000", None).await;
        assert_host_refused(status, &resp, &format!("{method} {path}"));
    }
    // The exempt set is exempt on purpose: none of them serves corpus
    // bytes to a refused Host.
    for path in EXEMPT_TOP_LEVEL {
        let (status, resp) = raw(addr, "GET", path, "attacker.example:4000", None).await;
        assert!(
            !resp.contains(kb_server::middleware::ERR_HOST_REFUSED),
            "{path} is on the exempt list but returned the guard's refusal (status {status})"
        );
    }
}

/// A1-5. The permalink shell reads the corpus (OpenGraph title/summary,
/// moved-path 301), so it must not be a metadata oracle for a rebound page.
/// Needs a real SPA shell (a fixture dist), because without one every
/// fallback is `spa-unavailable` and the test would pass vacuously: the
/// positive control (an ADMITTED Host sees `og:title`) proves the channel
/// exists in this harness, and the rebound Host must get none of it.
#[tokio::test]
async fn permalink_shell_is_not_a_metadata_oracle_for_a_rebound_host() {
    let dist = tempfile::tempdir().unwrap();
    std::fs::write(
        dist.path().join("index.html"),
        "<html><head><title>kb</title></head><body><div id=\"root\"></div></body></html>",
    )
    .unwrap();
    let (_tmp, addr) = boot_with(
        false,
        ServerSection::default(),
        Some(dist.path().to_path_buf()),
    )
    .await;
    let body = |r: &str| r.split("\r\n\r\n").nth(1).unwrap_or("").to_string();

    // positive control: wait for the seed doc to be indexed
    let mut admitted = String::new();
    for _ in 0..80 {
        let (_, r) = raw(addr, "GET", "/a/smoke/seed.md", "localhost", None).await;
        admitted = body(&r);
        if admitted.contains("og:title") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert!(
        admitted.contains("og:title"),
        "control: an admitted Host must see the OG splice, got: {admitted}"
    );

    let (s1, hit) = raw(
        addr,
        "GET",
        "/a/smoke/seed.md",
        "attacker.example:4000",
        None,
    )
    .await;
    let (s2, miss) = raw(
        addr,
        "GET",
        "/a/smoke/nope.md",
        "attacker.example:4000",
        None,
    )
    .await;
    assert_eq!(s1, s2, "status must not distinguish a real doc from a miss");
    assert_eq!(
        body(&hit),
        body(&miss),
        "shell bytes must not depend on the path"
    );
    assert!(
        !body(&hit).contains("og:title"),
        "a refused Host must not receive OpenGraph corpus metadata"
    );
}

/// A1-1 end to end through the real router and `from_server_section`: a
/// same-host TLS proxy (peer 127.0.0.1) forwarding the published name.
#[tokio::test]
async fn parent_origin_host_is_admitted_through_real_router() {
    let server = ServerSection {
        parent_origin: "https://kb.example.com".to_string(),
        ..ServerSection::default()
    };
    let (_tmp, addr) = boot_with(false, server, None).await;
    let (status, resp) = raw(addr, "GET", "/api/identity", "kb.example.com", None).await;
    assert_eq!(
        status, 200,
        "the parent_origin host must be admitted: {resp}"
    );
    let (status, resp) = raw(addr, "GET", "/api/identity", "attacker.example", None).await;
    assert_host_refused(
        status,
        &resp,
        "an unlisted name beside a configured parent_origin",
    );
}
