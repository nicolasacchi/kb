//! O9 + O10 — the two holes in the SEC-02 `Host` guard recorded in
//! `docs/research/kb-adversarial-review-2026-09-30.md`.
//!
//! WHY these are their own file and not more cases in `middleware.rs`'s
//! unit tests: both findings are about the ADMISSION TABLE and about
//! WHERE THE NAME IS READ, and both were invisible to the table tests
//! that already existed — which is the point. The table tests asserted
//! the four loopback spellings they were written with, so every extra
//! spelling was a 403 that no assertion could see; and they called
//! `host_allowed(Some(host))` directly, which is exactly the shape that
//! cannot express "the header was absent".
//!
//! Three of the cases below (`localhost.`, `127.0.0.2`, `*.localhost`)
//! are O9, one (`no Host`, authority only) is O10, and two are the
//! regressions the fixes must NOT cause: `0.0.0.0` / `[::]` stay
//! refused, and a listed name arriving in `:authority` is still
//! admitted.

use axum::body::{to_bytes, Body};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use axum::Router;
use kb_core::config::ServerSection;
use kb_server::middleware::{
    effective_host_name, host_allowed, host_guard, is_loopback_host_label, normalize_host_label,
    split_host_port, ERR_HOST_REFUSED,
};
use kb_server::state::OriginConfig;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tower::ServiceExt;

/// Resolve an `OriginConfig` the way the DAEMON does — through
/// `from_server_section`, not by filling the fields in by hand. The
/// adversarial review records a fix that repaired only a test-only
/// constructor while the production one still disagreed; building the
/// config through the boot path is what makes that class of fix fail
/// here instead of passing.
fn cfg(hostnames: &[&str], addr: &str) -> Arc<OriginConfig> {
    Arc::new(OriginConfig::from_server_section(
        &ServerSection {
            addr: addr.to_string(),
            hostnames: hostnames.iter().map(|s| s.to_string()).collect(),
            ..ServerSection::default()
        },
        Arc::new(Vec::new()),
    ))
}

// --- O9: loopback spellings -------------------------------------------------

/// The rule is "does this name denote loopback on this box", so the
/// whole of `127.0.0.0/8` is admitted, not just `127.0.0.1`. Before
/// this, `http://127.0.0.2:4000` on the box that serves it was a 403 on
/// every `/api` call while the SPA shell still loaded (the top-level
/// fallback is deliberately unguarded) — a page that renders and then
/// fails every fetch, and no boot warning to explain it, because the
/// bind IS loopback and `KB_ALLOW_NO_AUTH` is unset.
#[test]
fn the_whole_loopback_cidr_is_admitted_and_the_wildcard_is_not() {
    let p = cfg(&[], "127.0.0.1:4000");
    for h in [
        "127.0.0.1",
        "127.0.0.2",
        "127.1.2.3",
        // The far edge of the /8 — the boundary a hand-written
        // `127.0.0.1` list silently gets wrong.
        "127.255.255.254",
        "[::1]",
        "::1",
    ] {
        // The predicate takes a LABEL (no port); the request shape is
        // asserted through `host_allowed` below, which is the split
        // plus this predicate.
        assert!(is_loopback_host_label(h), "{h} is loopback");
    }
    for h in [
        "127.0.0.1",
        "127.0.0.2:4747",
        "127.1.2.3:4000",
        "127.255.255.254:4000",
        "[::1]:4747",
        "::1",
    ] {
        assert!(host_allowed(Some(h), &p), "{h} must be admitted");
    }
    // One below the /8, and the unspecified address, are not loopback.
    for h in ["126.255.255.255", "128.0.0.1", "0.0.0.0", "[::]", "::"] {
        assert!(!is_loopback_host_label(h), "{h} is not loopback");
    }
    for h in [
        "126.255.255.255:4000",
        "128.0.0.1:4000",
        "0.0.0.0",
        "0.0.0.0:4000",
        "[::]",
        "[::]:4000",
        "::",
        "::1x",
    ] {
        assert!(!host_allowed(Some(h), &p), "{h} must be refused");
    }
}

/// `localhost.` is the DNS root form of `localhost` (RFC 6761 §6.3) —
/// the same name, and a form an operator types. It used to work (there
/// was no `Host` check before SEC-02) and the guard turned it into a
/// 403 on the whole `/api` surface. Canonicalising the root label in
/// the PARSER is what fixes it for both a request label and a config
/// entry, so the two can never mean different things.
#[test]
fn a_trailing_root_dot_is_the_same_name_on_both_sides() {
    let p = cfg(&[], "127.0.0.1:4000");
    for h in [
        "localhost.",
        "localhost.:4747",
        "LOCALHOST.:4000",
        "127.0.0.1.:4000",
    ] {
        assert!(host_allowed(Some(h), &p), "{h} must be admitted");
    }
    assert_eq!(
        split_host_port("localhost.:4747"),
        Some(("localhost".to_string(), Some("4747")))
    );
    assert_eq!(
        split_host_port("kbc.example.com."),
        Some(("kbc.example.com".to_string(), None))
    );
    // A label that is nothing but dots is unparseable, and an
    // unparseable name is REFUSED — the root-dot rule must not become a
    // way to send an empty label.
    assert_eq!(split_host_port("."), None);
    assert!(!host_allowed(Some("."), &p));
    // And it cannot launder an attacker's name: the dot comes off, and
    // what is left is still not on the list.
    assert!(!host_allowed(Some("attacker.example."), &p));
    // Config side: the entry and the request canonicalise identically.
    assert_eq!(
        normalize_host_label("kbc.example.com."),
        Some("kbc.example.com".to_string())
    );
    let dotty = cfg(&["kbc.example.com."], "127.0.0.1:4000");
    assert!(host_allowed(Some("kbc.example.com"), &dotty));
    assert!(host_allowed(Some("kbc.example.com."), &dotty));
}

/// `*.localhost` is REFUSED, deliberately — the one O9 spelling this
/// does not admit, and the reasoning is the point:
///
///   * it is a RESOLVER convention, not a DNS fact. Chrome, Firefox,
///     glibc ≥ 2.35 and systemd-resolved force `foo.localhost` to
///     loopback, but on a box without that — musl, an older libc, or a
///     `search` domain — `foo.localhost` can fall through to a public
///     name the attacker owns. An allowlist must not admit a name whose
///     loopback-ness depends on who resolved it.
///   * the only `*.localhost` in this project is the artifact-iframe
///     host, `<id>.artifacts.localhost`, which must stay refused at
///     `/api` (the iframe is served by the unguarded top-level
///     fallback; admitting the name here would hand every artifact
///     bundle an `/api` route).
///
/// The operator who wants that name is not stuck: `[server] hostnames`
/// is the documented way to add exactly this name, and it is pinned
/// here so the decision reads as a policy with a remedy rather than a
/// dead end.
#[test]
fn a_dot_localhost_alias_is_refused_unless_it_is_listed() {
    let p = cfg(&[], "127.0.0.1:4000");
    for h in [
        "kb.localhost",
        // Even the resolver-honoured, browser-forced spellings, with the
        // root dot and with a port.
        "kb.localhost.",
        "kitchen-sink.artifacts.localhost",
    ] {
        assert!(!is_loopback_host_label(h), "{h} is not a loopback label");
    }
    for h in [
        "kb.localhost",
        "kb.localhost:4000",
        "kb.localhost.:4000",
        "kitchen-sink.artifacts.localhost:4000",
    ] {
        assert!(!host_allowed(Some(h), &p), "{h} must be refused");
    }
    let listed = cfg(&["kb.localhost"], "127.0.0.1:4000");
    assert!(host_allowed(Some("kb.localhost:4747"), &listed));
    // Listing it does not drag the artifact set in with it: that set is
    // unbounded, which is exactly why it is not listable.
    assert!(!host_allowed(
        Some("kitchen-sink.artifacts.localhost:4000"),
        &listed
    ));
}

/// O9 end to end, through the real middleware: a same-box browse of
/// these names reaches the handler, and the wildcard addresses are
/// still refused with the self-diagnosing urn.
#[tokio::test]
async fn the_loopback_spellings_reach_the_handler_and_the_wildcards_do_not() {
    let p = cfg(&[], "127.0.0.1:4000");
    for host in [
        "localhost:4000",
        "localhost.:4000",
        "127.0.0.1:4000",
        "127.0.0.2:4000",
        "[::1]:4000",
    ] {
        let resp = guarded(p.clone())
            .oneshot(req("127.0.0.1", "/kbs", Some(host)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{host} must be admitted");
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        assert_eq!(&body[..], b"kbs");
    }
    for host in ["0.0.0.0:4000", "[::]:4000", "kb.localhost:4000"] {
        let resp = guarded(p.clone())
            .oneshot(req("127.0.0.1", "/kbs", Some(host)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{host}");
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains(ERR_HOST_REFUSED), "{body}");
        // The operator can diagnose it from the response alone, and the
        // remedy is named even for a name this guard will never admit
        // implicitly.
        assert!(body.contains("[server] hostnames"), "{body}");
    }
}

// --- O10: the name is not always in the `Host` header ----------------------

/// THE O10 REGRESSION. `Host` is optional in HTTP/2 when `:authority`
/// is present (RFC 9113 §8.3.1), and a reverse proxy with an HTTP/2
/// upstream takes that option — so `/api` can be reached with no `Host`
/// header at all. Reading only the header meant `host_allowed(None)`,
/// which returns TRUE, decided the request: an operator who set
/// `hostnames = ["kb.example.com"]` on such a deploy got NO `Host`
/// enforcement, silently, and every request through the proxy passed
/// whatever name it carried.
///
/// The fix is not a blanket refusal either: a LISTED name arriving in
/// the authority is admitted, which is what a correct proxy sends.
#[tokio::test]
async fn an_absent_host_falls_back_to_the_request_authority() {
    let p = cfg(&["kb.example.com"], "127.0.0.1:4000");

    // No `Host` header, name only in the URI authority → refused.
    let resp = guarded(p.clone())
        .oneshot(req("127.0.0.1", "http://evil.example:4000/kbs", None))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "an HTTP/2 upstream carrying :authority must still be checked"
    );
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains(ERR_HOST_REFUSED), "{body}");

    // The same shape with a LISTED name → admitted. Without this the
    // "fix" would just be a new way to break the proxy deployment.
    let resp = guarded(p.clone())
        .oneshot(req("127.0.0.1", "http://kb.example.com/kbs", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"kbs");

    // `Host` wins when BOTH are present — and that precedence is pinned
    // deliberately, because the alternative is not obviously right. A
    // client CAN put a listed name in the header and anything at all in
    // the authority, and this admits it. It is still the right call:
    // RFC 9113 §8.3.1 requires a compliant client to make the two
    // agree, a browser derives its own origin from that same name, and
    // nothing else in the daemon reads the URI authority — so there is
    // nothing for a mismatch to smuggle past. The fallback exists for
    // the no-header case, so flipping the precedence would be a real
    // behaviour change; it belongs in a diff, not in a drive-by.
    let resp = guarded(p.clone())
        .oneshot(req(
            "127.0.0.1",
            "http://evil.example/kbs",
            Some("kb.example.com"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A request with no name AT ALL — no `Host`, no authority: an HTTP/1.0
/// client (RFC 9112 made `Host` mandatory in 1.1) or an in-process
/// `tower::oneshot` caller. It is admitted, and that is the deliberate
/// decision, not an oversight: rebinding needs a browser, a browser
/// always sends the page's name, and that name is checked. "No name" is
/// not a way to present a name that is not on the list — only a way to
/// present none — and refusing it would 403 every in-process client in
/// this repo's own suite for no security gain.
#[tokio::test]
async fn a_request_with_no_name_at_all_is_still_admitted() {
    let p = cfg(&["kb.example.com"], "127.0.0.1:4000");
    let resp = guarded(p)
        .oneshot(req("127.0.0.1", "/kbs", None))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(effective_host_name(&req("127.0.0.1", "/kbs", None)), None);
}

/// A `Host` header that is present but unreadable as text is a name we
/// cannot CHECK, so it is refused. The predecessor read it as
/// `headers().get(HOST).and_then(|v| v.to_str().ok())`, which collapsed
/// "cannot read the name" into "no name" — and no name means pass. That
/// is a header the client fully controls acting as its own bypass.
#[tokio::test]
async fn a_host_header_we_cannot_read_is_refused_not_treated_as_absent() {
    let p = cfg(&[], "127.0.0.1:4000");
    let opaque = axum::http::HeaderValue::from_bytes(b"\xff\xfe").unwrap();
    let request = Request::builder()
        .method(Method::GET)
        .uri("/kbs")
        .extension(ConnectInfo(SocketAddr::new(
            "127.0.0.1".parse::<IpAddr>().unwrap(),
            50000,
        )))
        .header("host", opaque)
        .body(Body::empty())
        .unwrap();
    // The lookup itself, not just the status: the fallback must not
    // present an unreadable header as "absent".
    assert_eq!(effective_host_name(&request), Some(""));
    let resp = guarded(p).oneshot(request).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// --- harness ---------------------------------------------------------------

/// The guard on a two-route tree, driven through tower's `oneshot` with
/// a wired loopback `ConnectInfo` — the shape `build_router` produces
/// for a same-box client, and therefore the shape `host_gate_applies`
/// gates unconditionally (no configuration required).
fn guarded(origin_cfg: Arc<OriginConfig>) -> Router {
    Router::new()
        .route("/kbs", get(|| async { "kbs" }))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(axum::middleware::from_fn_with_state(origin_cfg, host_guard))
}

fn req(peer: &str, uri: &str, host: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .extension(ConnectInfo(SocketAddr::new(
            peer.parse::<IpAddr>().unwrap(),
            50000,
        )));
    if let Some(h) = host {
        b = b.header("host", h);
    }
    b.body(Body::empty()).unwrap()
}
