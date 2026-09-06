//! SEC-02 — the Origin + Host allowlist and the non-simple mutation
//! header.
//!
//! # Why not reuse `kb_server::middleware::origin_allowlist`?
//!
//! It is `pub`, but it is not the same guard. kb-server's version (a) is
//! stated over its OWN `OriginConfig` (`parent_origin` +
//! `artifact_host_suffix` — two concepts kb-code has no analogue for), (b)
//! checks `Origin` only on state-changing methods, and (c) never checks
//! `Host` at all. kb-code needs `Host` checked on EVERY method (that is
//! the whole DNS-rebinding defence — a rebound page's reads are as
//! damaging as its writes, since `/api/file` serves the operator's repos)
//! and needs the allowlist stated over `[server] hostnames` + the doc-lens
//! CORS origins. So this is the equivalent, not a copy: same shape, same
//! "absent Origin passes" rule, same `Origin == Host` same-origin arm —
//! kb-code's own inputs. `kb_server::middleware::request_is_loopback` IS
//! reused verbatim (invariant #4 says never re-derive that predicate).
//!
//! # The admission table
//!
//! Both middlewares are layered OUTERMOST on the whole `/api` nest
//! (chained last in `router::build_router`), so they decide before
//! `auth_bearer` / `loopback_only` / `review_mutations_gate` ever run. A
//! request that passes both reaches the byte-identical pre-V70-A2 stack.
//!
//! ## Host ([`host_allowed`])
//!
//! Allowed: `localhost`, `127.0.0.1`, `[::1]`, `::1` (ANY port, and an
//! absent `Host` — HTTP/2 requests carry `:authority`, which hyper folds
//! into `Host` for us, but an in-process test client may send neither),
//! plus every entry in `[server] hostnames`.
//!
//! Enforced when the peer is loopback, OR when `[server] hostnames` is
//! non-empty. That carve-out is deliberate and is the ONLY place this
//! module fails open:
//!
//! * The rebinding attack is a LOOPBACK-peer attack. The victim's browser
//!   resolves `attacker.com` to `127.0.0.1`, connects from the box, and
//!   the daemon's loopback bypass hands it operator authority with no
//!   credential. `Host: attacker.com` is refused here, unconditionally,
//!   with no config needed. That is SEC-02's actual hole and it is closed
//!   by default.
//! * A non-loopback peer is, by construction, arriving through a reverse
//!   proxy (prod `kbc.example.com`: Traefik → Authelia → an injected bearer)
//!   and is already gated by `auth_bearer`. Refusing its `Host` before the
//!   operator has had a chance to write `hostnames = ["kbc.example.com"]` would
//!   take a deployed daemon down on upgrade — a hardening unit that bricks
//!   the deployment it hardens is not a hardening unit. [`HostPolicy::
//!   warn_if_unconfigured`] logs one boot warning naming the key, and the
//!   moment `hostnames` is set the check applies to every peer.
//!
//! ## Origin ([`origin_allowed`])
//!
//! An ABSENT `Origin` passes (curl, `kb-code`, in-process tests — the same
//! rule and the same rationale kb-server's `origin_allowlist` records; a
//! malicious local process needs no browser). A PRESENT `Origin` must be
//! one of:
//!
//! * a loopback origin or a `[server] hostnames` entry (the host allowlist
//!   above, matched on the origin's host, any port);
//! * byte-equal to the request's own `Host` (same-origin — the SPA the
//!   daemon itself served, whatever hostname it was reached on);
//! * one of `[doclens] origins` — the doc↔code bridge's EXPLICIT,
//!   operator-configured cross-origin allowlist (`doclens::cors`). Those
//!   routes are the one part of `/api` that is meant to be reachable from
//!   kb's own SPA on a different origin; refusing them here would break
//!   DCB W1.C/W2.A while `CorsLayer` still advertised them.
//!
//! ## `X-Kbc-Request: 1` ([`mutation_header_guard`])
//!
//! Required on `POST`/`PUT`/`PATCH`/`DELETE` under `/api`. This is the
//! critique's FIX (b): a header no *simple* request can carry, so a
//! cross-origin page must first win a CORS preflight, and this daemon
//! answers preflights only on the doc-lens sub-routers.
//!
//! It is what actually stops the residual case the Origin allowlist can
//! not: `http://localhost:3000` — some OTHER dev server's page on the same
//! box — IS a loopback origin, so it passes the allowlist by design (that
//! keeps read admission byte-identical to today). It cannot mutate,
//! because its `fetch` with `X-Kbc-Request` preflights and gets no CORS
//! answer, and its `fetch` without the header is refused here.
//!
//! By default the header is required only when the request carries an
//! `Origin` — i.e. of BROWSER-originated mutations. Every browser sends
//! `Origin` on every cross-origin `fetch`/`XHR` and on cross-origin form
//! POSTs, so no browser vector escapes; a local non-browser process is
//! explicitly out of scope (kb-server's deep-review M6 ruling: *"a
//! malicious LOCAL process can already do anything else on the user's
//! machine"*), and keeping Origin-less callers unchanged is what lets the
//! CLI, curl and this crate's ~350 integration-test clients keep working
//! byte-identically. `[security] strict_request_header = true` extends the
//! requirement to Origin-less requests too — one flag away, exactly the
//! posture kb-cli's own `X-Requested-By` header records.
//!
//! ## Why no separate CSRF token
//!
//! The critique's FIX (c) suggests minting a CSRF token into the SPA
//! bootstrap. A token would add nothing over the pair above, and would add
//! a real cost:
//!
//! * A CSRF token defends against a cross-origin page forging a request.
//!   Such a page cannot set `X-Kbc-Request` without a preflight, and it
//!   cannot win a preflight (no CORS answer on any mutating route outside
//!   the operator-configured doc-lens pin). The header IS the
//!   unguessable-to-a-cross-origin-page credential, structurally, with no
//!   state to mint, store, rotate or leak.
//! * The remaining threat the token *would* also fail against is a
//!   SAME-ORIGIN attacker (XSS in the SPA): it can read the token out of
//!   the bootstrap exactly as easily as it can set a header. A CSRF token
//!   is not an XSS defence — CSP is (see [`crate::spa`]).
//! * A token in the SPA bootstrap must be readable by the page, therefore
//!   must survive the shell's cache, therefore becomes a value that
//!   outlives a rotation — a durable secret the daemon does not otherwise
//!   have.
//!
//! Recorded here rather than in a design doc because the next person to
//! read `router.rs` and notice "there is no CSRF token" should find the
//! reasoning next to the code that replaces it.

use crate::config::{KbCodeConfig, SecuritySection};
use crate::state::SharedState;
use axum::{
    body::Body,
    http::{header, HeaderValue, Method, Request, Response, StatusCode},
    middleware::Next,
    response::IntoResponse,
};

/// The `X-Kbc-Request` header name — a NON-simple header (not in the CORS
/// safelist), which is the entire point: sending it cross-origin forces a
/// preflight.
pub const REQUEST_HEADER: &str = "x-kbc-request";

/// The only value accepted for [`REQUEST_HEADER`]. A fixed `1` rather than
/// a free-form marker so a future value can mean a new protocol revision.
pub const REQUEST_HEADER_VALUE: &str = "1";

/// `urn:kb:errors:origin-refused` — the `Origin`/`Host` refusal.
pub const ERR_ORIGIN_REFUSED: &str = "urn:kb:errors:origin-refused";
/// `urn:kb:errors:missing-request-header` — the mutation-header refusal.
pub const ERR_MISSING_REQUEST_HEADER: &str = "urn:kb:errors:missing-request-header";

/// Routes exempt from the [`mutation_header_guard`], as `/api`-relative
/// path prefixes.
///
/// ONE entry, and it is a considered exemption rather than a convenience:
/// `PUT`/`DELETE /api/doc-lens/pin` is driven CROSS-ORIGIN by **kb's own
/// reader SPA** (a different crate's frontend, DCB decision D-A — "the pin
/// IS the remembered read-time choice"), which this unit does not touch.
/// That route is already the most tightly origin-scoped mutation in the
/// router: it answers only for `[doclens] origins`, an explicit
/// operator-configured allowlist, and its `Content-Type: application/json`
/// body already forces a preflight that `doclens::cors::pin_cors` alone
/// decides. Requiring a header its only cross-origin caller does not send
/// would break the DCB pin lane while adding no guard the CORS allowlist
/// does not already provide.
///
/// `security_exempt_route_set_is_pinned` (tests/security) asserts this
/// exact set, so a later route cannot join it by accident — the same
/// contract `cors_layer_route_set_is_pinned` holds for the ACAO set.
pub const MUTATION_HEADER_EXEMPT_PREFIXES: &[&str] = &["/api/doc-lens/pin"];

/// The resolved Host/Origin allowlist, built once at boot from config and
/// parked on [`crate::state::AppState`] (no live reload — same posture as
/// every other config clone on that struct).
#[derive(Debug, Clone, Default)]
pub struct HostPolicy {
    /// `[server] hostnames`, lowercased. Empty = "unconfigured", which
    /// switches Host enforcement to loopback-peers-only (module doc).
    hostnames: Vec<String>,
    /// `[doclens] origins`, already normalized by
    /// `DoclensSection::normalized_origins`.
    doclens_origins: Vec<String>,
    /// `[security] strict_request_header`.
    strict_request_header: bool,
}

impl HostPolicy {
    pub fn from_config(config: &KbCodeConfig) -> Self {
        Self {
            hostnames: config
                .server
                .hostnames
                .iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
            doclens_origins: config.doclens.normalized_origins(),
            strict_request_header: config.security.strict_request_header,
        }
    }

    /// Test/fixture constructor — the pieces, without a whole config.
    pub fn new(
        hostnames: Vec<String>,
        doclens_origins: Vec<String>,
        security: &SecuritySection,
    ) -> Self {
        Self {
            hostnames: hostnames
                .iter()
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect(),
            doclens_origins,
            strict_request_header: security.strict_request_header,
        }
    }

    pub fn strict_request_header(&self) -> bool {
        self.strict_request_header
    }

    /// True when `[server] hostnames` is configured — Host checking then
    /// applies to every peer, not only loopback ones (module doc).
    pub fn hostnames_configured(&self) -> bool {
        !self.hostnames.is_empty()
    }

    /// One boot warning when a daemon binds a NON-loopback address with no
    /// `hostnames` configured — the deployment shape where the Host guard
    /// is in its fail-open mode. Called from `bind_and_spawn`.
    pub fn warn_if_unconfigured(&self, local_is_loopback: bool) {
        if !local_is_loopback && !self.hostnames_configured() {
            tracing::warn!(
                "kb-code bound a non-loopback address with no `[server] hostnames`; \
                 the Host allowlist is enforced on loopback peers only. Set \
                 `[server] hostnames = [\"<public-host>\"]` for strict Host checking."
            );
        }
    }
}

/// A hostname (no scheme, possibly with a port, possibly bracketed IPv6)
/// split into its host label and optional port. `None` for an unparseable
/// value.
fn split_host_port(value: &str) -> Option<(String, Option<&str>)> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Some(rest) = v.strip_prefix('[') {
        // `[::1]` / `[::1]:4747` — keep the brackets in the host label so
        // it compares equal to the `[::1]` spelling operators write.
        let (inner, tail) = rest.split_once(']')?;
        let port = tail.strip_prefix(':');
        return Some((format!("[{}]", inner.to_ascii_lowercase()), port));
    }
    match v.rsplit_once(':') {
        // An unbracketed value with MORE than one colon is a bare IPv6
        // literal (`::1`), not host:port.
        Some((host, port)) if !host.contains(':') => Some((host.to_ascii_lowercase(), Some(port))),
        _ => Some((v.to_ascii_lowercase(), None)),
    }
}

/// The always-allowed loopback host labels, in every spelling a client can
/// send one.
fn is_loopback_host_label(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
}

/// `Host:` admission — see the module doc. `None` (header absent) passes:
/// HTTP/1.0 and some in-process clients send no Host, and refusing them
/// would change admission for callers that were never a rebinding vector.
pub fn host_allowed(host: Option<&str>, policy: &HostPolicy) -> bool {
    let Some(host) = host else {
        return true;
    };
    let Some((label, _port)) = split_host_port(host) else {
        return false;
    };
    is_loopback_host_label(&label) || policy.hostnames.contains(&label)
}

/// `Origin:` admission — see the module doc.
pub fn origin_allowed(origin: &str, host: Option<&str>, policy: &HostPolicy) -> bool {
    let normalized = origin.trim().to_ascii_lowercase();
    // An explicitly configured doc-lens origin, matched whole (scheme
    // included) exactly the way `doclens::cors` matches it.
    if policy.doclens_origins.contains(&normalized) {
        return true;
    }
    let Some(rest) = normalized
        .strip_prefix("http://")
        .or_else(|| normalized.strip_prefix("https://"))
    else {
        // `null`, `file://`, or anything else that is not an http(s)
        // origin — never a caller this daemon serves.
        return false;
    };
    // Same-origin: the SPA the daemon itself served back carries
    // `Origin: <scheme>://<Host>`, whatever hostname it was reached on.
    if let Some(host) = host {
        if rest == host.trim().to_ascii_lowercase() {
            return true;
        }
    }
    let Some((label, _port)) = split_host_port(rest) else {
        return false;
    };
    is_loopback_host_label(&label) || policy.hostnames.contains(&label)
}

/// Layer 1 (outermost) — the Origin + Host allowlist, on EVERY `/api`
/// route including the loopback-only and `review_remote` sub-routers.
pub async fn origin_host_guard(
    axum::extract::State(state): axum::extract::State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    let policy = &state.host_policy;
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let peer_is_loopback =
        kb_server::middleware::request_is_loopback(&req, &state.auth.trusted_proxies);

    // Host: enforced for a loopback peer always (the rebinding case), and
    // for every peer once `[server] hostnames` is configured.
    if (peer_is_loopback || policy.hostnames_configured()) && !host_allowed(host.as_deref(), policy)
    {
        return refuse(
            ERR_ORIGIN_REFUSED,
            "Host refused",
            format!(
                "Host {:?} is not in this daemon's allowlist. Add it to `[server] hostnames`.",
                host.unwrap_or_default()
            ),
        );
    }

    if let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
    {
        if !origin_allowed(&origin, host.as_deref(), policy) {
            return refuse(
                ERR_ORIGIN_REFUSED,
                "Origin refused",
                format!("Origin {origin:?} is not in this daemon's allowlist."),
            );
        }
    }

    next.run(req).await
}

/// True for the methods that mutate. Mirrors
/// `kb_server::middleware::is_state_changing` — `OPTIONS` is deliberately
/// NOT in the set (a preflight is the legitimate cross-origin OPTIONS, and
/// `CorsLayer` owns that dance).
fn is_mutating(method: &Method) -> bool {
    matches!(
        method,
        &Method::POST | &Method::PUT | &Method::PATCH | &Method::DELETE
    )
}

/// True when `path` is one of the [`MUTATION_HEADER_EXEMPT_PREFIXES`].
pub fn is_mutation_header_exempt(path: &str) -> bool {
    MUTATION_HEADER_EXEMPT_PREFIXES
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
}

/// Layer 2 — the `X-Kbc-Request: 1` requirement on mutating `/api` routes.
/// See the module doc for the Origin-gating default and the strict opt-in.
pub async fn mutation_header_guard(
    axum::extract::State(state): axum::extract::State<SharedState>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    // `full_path`, NOT `req.uri().path()` — these guards run inside the
    // `/api` nest, which strips the prefix (see that fn's doc).
    if !is_mutating(req.method()) || is_mutation_header_exempt(&crate::security::full_path(&req)) {
        return next.run(req).await;
    }
    let browser_originated = req.headers().contains_key(header::ORIGIN);
    if !browser_originated && !state.host_policy.strict_request_header() {
        return next.run(req).await;
    }
    let ok = req
        .headers()
        .get(REQUEST_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim() == REQUEST_HEADER_VALUE)
        .unwrap_or(false);
    if ok {
        return next.run(req).await;
    }
    refuse(
        ERR_MISSING_REQUEST_HEADER,
        "Missing request header",
        format!(
            "mutating requests must carry `{}: {}` (a non-simple header, so a \
             cross-origin page cannot send it without a preflight this daemon \
             does not answer).",
            "X-Kbc-Request", REQUEST_HEADER_VALUE
        ),
    )
}

/// An RFC 7807 `application/problem+json` 403.
///
/// Deliberately NOT `routes::ApiError`'s `{"error": ...}` shape: this is a
/// SECURITY refusal from a middleware that runs before any handler, and it
/// carries a stable machine `type` the SPA and CLI branch on.
fn refuse(urn: &'static str, title: &'static str, detail: String) -> Response<Body> {
    let body = serde_json::json!({
        "type": urn,
        "title": title,
        "status": 403,
        "detail": detail,
    });
    let mut resp = (StatusCode::FORBIDDEN, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(hostnames: &[&str], doclens: &[&str]) -> HostPolicy {
        HostPolicy::new(
            hostnames.iter().map(|s| s.to_string()).collect(),
            doclens.iter().map(|s| s.to_string()).collect(),
            &SecuritySection::default(),
        )
    }

    #[test]
    fn loopback_hosts_are_allowed_on_any_port_with_no_config() {
        let p = policy(&[], &[]);
        for h in [
            "localhost",
            "localhost:4747",
            "127.0.0.1",
            "127.0.0.1:4747",
            "[::1]",
            "[::1]:4747",
            "::1",
            "LOCALHOST:4747",
        ] {
            assert!(host_allowed(Some(h), &p), "{h} must be allowed");
        }
        // Absent Host passes — see `host_allowed`'s doc.
        assert!(host_allowed(None, &p));
    }

    #[test]
    fn a_rebound_hostname_is_refused() {
        let p = policy(&[], &[]);
        for h in ["attacker.com", "attacker.com:4747", "kbc.example.com"] {
            assert!(!host_allowed(Some(h), &p), "{h} must be refused");
        }
    }

    #[test]
    fn configured_hostnames_join_the_allowlist() {
        let p = policy(&["kbc.example.com"], &[]);
        assert!(host_allowed(Some("kbc.example.com"), &p));
        assert!(host_allowed(Some("KBC.EXAMPLE.COM:443"), &p));
        assert!(!host_allowed(Some("evil.example.com"), &p));
    }

    #[test]
    fn origin_admission_covers_loopback_same_origin_and_doclens() {
        let p = policy(&["kbc.example.com"], &["https://kb.example.com"]);
        // Loopback origins (any port) — read admission stays byte-identical.
        assert!(origin_allowed("http://localhost:4747", None, &p));
        assert!(origin_allowed("http://127.0.0.1:5173", None, &p));
        assert!(origin_allowed("http://localhost:3000", None, &p));
        // Same-origin against the request's own Host.
        assert!(origin_allowed(
            "https://kbc.example.com",
            Some("kbc.example.com"),
            &p
        ));
        // Configured hostname, even when Host says otherwise.
        assert!(origin_allowed(
            "https://kbc.example.com",
            Some("localhost"),
            &p
        ));
        // The doc-lens cross-origin allowlist.
        assert!(origin_allowed("https://kb.example.com", None, &p));
        // Everything else.
        assert!(!origin_allowed("https://evil.example", None, &p));
        assert!(!origin_allowed("null", None, &p));
        assert!(!origin_allowed("file://", None, &p));
    }

    #[test]
    fn split_host_port_handles_ipv6_and_ports() {
        assert_eq!(
            split_host_port("[::1]:4747"),
            Some(("[::1]".to_string(), Some("4747")))
        );
        assert_eq!(split_host_port("::1"), Some(("::1".to_string(), None)));
        assert_eq!(
            split_host_port("localhost:4747"),
            Some(("localhost".to_string(), Some("4747")))
        );
        assert_eq!(split_host_port(""), None);
    }

    #[test]
    fn only_the_doc_lens_pin_is_exempt_from_the_mutation_header() {
        assert_eq!(MUTATION_HEADER_EXEMPT_PREFIXES, &["/api/doc-lens/pin"]);
        assert!(is_mutation_header_exempt("/api/doc-lens/pin"));
        assert!(!is_mutation_header_exempt("/api/doc-lens/pins"));
        assert!(!is_mutation_header_exempt("/api/doc-lens/sync"));
        assert!(!is_mutation_header_exempt("/api/checkout"));
    }

    #[test]
    fn only_state_changing_methods_need_the_header() {
        assert!(is_mutating(&Method::POST));
        assert!(is_mutating(&Method::PUT));
        assert!(is_mutating(&Method::PATCH));
        assert!(is_mutating(&Method::DELETE));
        assert!(!is_mutating(&Method::GET));
        assert!(!is_mutating(&Method::HEAD));
        // A preflight must never be refused here — `CorsLayer` owns it.
        assert!(!is_mutating(&Method::OPTIONS));
    }
}
