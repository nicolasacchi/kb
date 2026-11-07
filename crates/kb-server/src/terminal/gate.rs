//! The terminal gate: enabled → Origin → intent → caller class, in that
//! order, run **in the handler**, not as router middleware.
//!
//! `origin_allowlist` skips GETs and passes an absent Origin (a
//! WebSocket upgrade is a GET, and a curl POST with no Origin passes);
//! the loopback bypass in `auth_bearer` admits any loopback caller and
//! agents run on loopback. Admission by `auth_bearer` is therefore
//! necessary, never sufficient: this gate fails closed on every axis
//! the middleware does not check.
//!
//! T12's WebSocket handler calls [`check_origin`] directly for the
//! upgrade (which also needs the ticket consumer); the REST handlers
//! call [`gate`] first.

use axum::body::Body;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::middleware::{Identity, IdentitySource};
use crate::state::{KbHandles, OriginConfig};

/// `urn:kb:errors:terminal-origin-refused` — the Origin refusal.
pub(crate) const ERR_TERMINAL_ORIGIN_REFUSED: &str = "urn:kb:errors:terminal-origin-refused";
/// `urn:kb:errors:terminal-intent-required` — the intent refusal.
pub(crate) const ERR_TERMINAL_INTENT_REQUIRED: &str = "urn:kb:errors:terminal-intent-required";
/// `urn:kb:errors:terminal-token-refused` — the token-caller refusal.
pub(crate) const ERR_TERMINAL_TOKEN_REFUSED: &str = "urn:kb:errors:terminal-token-refused";
/// `urn:kb:errors:bad-request` — a malformed terminal request body or
/// an invalid terminal id.
pub(crate) const ERR_TERMINAL_BAD_REQUEST: &str = "urn:kb:errors:bad-request";
/// `urn:kb:errors:terminal-runner-unavailable` — the runner is down.
pub(crate) const ERR_TERMINAL_RUNNER_UNAVAILABLE: &str =
    "urn:kb:errors:terminal-runner-unavailable";
/// `urn:kb:errors:terminal-not-found` — the runner has no such
/// terminal.
pub(crate) const ERR_TERMINAL_NOT_FOUND: &str = "urn:kb:errors:terminal-not-found";
/// `urn:kb:errors:terminal-conflict` — the runner reports a conflict
/// (`blocked`, `grid_owned`).
pub(crate) const ERR_TERMINAL_CONFLICT: &str = "urn:kb:errors:terminal-conflict";
/// `urn:kb:errors:terminal-refused` — the runner refuses the operation
/// (`multi_line_refused`).
pub(crate) const ERR_TERMINAL_REFUSED: &str = "urn:kb:errors:terminal-refused";
/// `urn:kb:errors:terminal-bad-reply` — anything else from the runner.
pub(crate) const ERR_TERMINAL_BAD_REPLY: &str = "urn:kb:errors:terminal-bad-reply";
/// `urn:kb:errors:payload-too-large` — a request body or paste text the
/// relay refuses before parsing (same urn as the rest of the daemon).
pub(crate) const ERR_TERMINAL_PAYLOAD_TOO_LARGE: &str = "urn:kb:errors:payload-too-large";

/// The SPA-only intent header. An INTENT marker, not an authorization:
/// kb has one trust tier and a local non-browser client can forge both
/// the Origin and this header, so what it buys is that a cross-origin
/// page cannot send a POST (the only CORS layer is GET-only) and an
/// agent holding a terminal id cannot type into a terminal without
/// having been told to. Copy of the `x-kb-visibility` precedent.
pub(crate) const TERMINAL_INTENT_HEADER: &str = "x-kb-intent";

/// True iff the request carries `X-Kb-Intent: terminal` (ASCII
/// case-insensitive, trimmed), so `Terminal` and `TERMINAL` also pass.
pub(crate) fn terminal_intent(headers: &HeaderMap) -> bool {
    headers
        .get(TERMINAL_INTENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("terminal"))
}

/// Whether `origin` is acceptable for the terminal tier.
///
/// Accept ONLY exact equality with `cfg.parent_origin` (plain string
/// equality, no normalisation), or `http://H`/`https://H` where `H`
/// equals the request `Host` header exactly (a missing Host refuses).
/// The artifact-suffix arm of `origin_allowed` and `KB_DEV_ORIGIN_ANY`
/// are deliberately NOT accepted — artifact subdomains run untrusted
/// HTML and must never reach the terminal tier.
///
/// T12's WS handler calls this same function at the upgrade.
pub(crate) fn check_origin(headers: &HeaderMap, cfg: &OriginConfig) -> Result<(), GateRefusal> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .ok_or(GateRefusal::Origin)?;
    if origin == cfg.parent_origin {
        return Ok(());
    }
    if let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    {
        if let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) {
            if rest == host {
                return Ok(());
            }
        }
    }
    Err(GateRefusal::Origin)
}

/// Why [`check_origin`] or [`gate`] refused a request. Typed so the
/// refusal mapping to a `problem+json` body stays in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateRefusal {
    Origin,
    Intent,
    Token,
}

impl GateRefusal {
    pub(crate) fn urn(self) -> &'static str {
        match self {
            GateRefusal::Origin => ERR_TERMINAL_ORIGIN_REFUSED,
            GateRefusal::Intent => ERR_TERMINAL_INTENT_REQUIRED,
            GateRefusal::Token => ERR_TERMINAL_TOKEN_REFUSED,
        }
    }

    pub(crate) fn detail(self) -> &'static str {
        match self {
            GateRefusal::Origin => "terminal routes require an exact parent or same-host Origin",
            GateRefusal::Intent => "terminal POSTs require `X-Kb-Intent: terminal`",
            GateRefusal::Token => "token-authenticated callers are refused on terminal routes",
        }
    }
}

/// A bodiless 404 for a disabled terminal tier: no JSON, no
/// content-type, no header naming the feature — a disabled daemon
/// leaks nothing about the terminal tier.
fn disabled() -> Response<Body> {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::empty())
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// The typed `problem+json` body of every terminal refusal. Deliberately
/// a local helper: `middleware::forbidden` is private, and this module
/// must not move the wire-ratchet `json_sites` ceiling (hence no
/// `json!` here). Sets `content-type: application/problem+json` and
/// `cache-control: no-store`.
pub(crate) struct Problem {
    pub urn: &'static str,
    pub status: StatusCode,
    pub title: &'static str,
    pub detail: String,
}

#[derive(Serialize)]
struct ProblemBody {
    r#type: &'static str,
    title: &'static str,
    status: u16,
    detail: String,
}

impl Problem {
    pub(crate) fn refusal(refusal: GateRefusal) -> Response<Body> {
        (Self {
            urn: refusal.urn(),
            status: StatusCode::FORBIDDEN,
            title: "Forbidden",
            detail: refusal.detail().to_string(),
        })
        .into_response()
    }

    pub(crate) fn response(
        urn: &'static str,
        status: StatusCode,
        title: &'static str,
        detail: String,
    ) -> Response<Body> {
        Self {
            urn,
            status,
            title,
            detail,
        }
        .into_response()
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response<Body> {
        let body = ProblemBody {
            r#type: self.urn,
            title: self.title,
            status: self.status.as_u16(),
            detail: self.detail,
        };
        let payload = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
        let mut resp = (
            self.status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            payload,
        )
            .into_response();
        resp.headers_mut().insert(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        );
        resp
    }
}

/// The gate every terminal handler calls first, in this order:
/// enabled → Origin → intent (POST only) → caller class. Returns the
/// refusal response on the first failure; `Ok` means the handler may
/// proceed. The enabled check runs BEFORE anything else so a disabled
/// daemon leaks nothing.
pub(crate) async fn gate(
    state: &KbHandles,
    headers: &HeaderMap,
    identity: &Identity,
    mutating: bool,
) -> Result<(), Response<Body>> {
    // 1. Enabled. A bodiless 404, deliberately unlike
    //    `dispatch::api_not_found` (problem+json) — the terminal tier
    //    must be invisible when off.
    let terminal = state.config.read().await.terminal.clone();
    if !terminal.enabled() {
        return Err(disabled());
    }

    // 2. Origin. T12's WS handler calls `check_origin` for the
    //    upgrade; REST shares it here. It does NOT call
    //    `origin_allowed`, so the artifact-suffix arm and
    //    `KB_DEV_ORIGIN_ANY` stay closed.
    check_origin(headers, &state.origin).map_err(Problem::refusal)?;

    // 3. Intent, POST only. GET routes (list/read/history) need the
    //    Origin but no intent header.
    if mutating && !terminal_intent(headers) {
        return Err(Problem::refusal(GateRefusal::Intent));
    }

    // 4. Caller class. The SPA is authenticated by loopback or the
    //    trusted-hop identity header (`Loopback`/`Header`); scripts
    //    and agents carry bearer or registry tokens.
    if terminal.refuse_token_callers()
        && matches!(
            identity.source,
            IdentitySource::Token | IdentitySource::Legacy
        )
    {
        return Err(Problem::refusal(GateRefusal::Token));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OriginConfig {
        OriginConfig {
            artifact_host_suffix: ".artifacts.localhost".to_string(),
            parent_origin: "http://localhost:4000".to_string(),
            trusted_proxies: crate::state::TrustedProxies::default(),
            hostnames: std::sync::Arc::new(Vec::new()),
            addr_host: None,
        }
    }

    fn headers(origin: Option<&str>, host: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(o) = origin {
            h.insert(header::ORIGIN, o.parse().unwrap());
        }
        if let Some(hh) = host {
            h.insert(header::HOST, hh.parse().unwrap());
        }
        h
    }

    #[test]
    fn check_origin_table() {
        let cfg = cfg();
        // Absent Origin → refuse.
        assert_eq!(
            check_origin(&headers(None, Some("x")), &cfg),
            Err(GateRefusal::Origin)
        );
        // Foreign origin → refuse.
        assert_eq!(
            check_origin(&headers(Some("https://evil.example"), Some("x")), &cfg),
            Err(GateRefusal::Origin)
        );
        // Artifact-suffix origin → refuse (the `origin_allowed`
        // artifact arm stays closed on the terminal tier).
        assert_eq!(
            check_origin(
                &headers(Some("http://x.artifacts.localhost"), Some("x")),
                &cfg
            ),
            Err(GateRefusal::Origin)
        );
        // Parent origin → ok.
        assert!(check_origin(&headers(Some("http://localhost:4000"), Some("x")), &cfg).is_ok());
        // Same Host as the Origin's authority → ok.
        assert!(check_origin(
            &headers(Some("http://127.0.0.1:4000"), Some("127.0.0.1:4000")),
            &cfg
        )
        .is_ok());
        // An Origin whose authority is not the Host → refuse.
        assert_eq!(
            check_origin(
                &headers(Some("http://127.0.0.1:4000"), Some("other.example")),
                &cfg
            ),
            Err(GateRefusal::Origin)
        );
        // Missing Host → refuse.
        assert_eq!(
            check_origin(&headers(Some("http://127.0.0.1:4000"), None), &cfg),
            Err(GateRefusal::Origin)
        );
    }

    #[test]
    fn origin_check_ignores_kb_dev_origin_any() {
        // The dev convenience must never open the terminal tier. Set,
        // check, restore. (nextest isolates processes; set_var is the
        // edition-2021 shape.)
        let cfg = cfg();
        let orig = std::env::var_os("KB_DEV_ORIGIN_ANY");
        std::env::set_var("KB_DEV_ORIGIN_ANY", "1");
        let refused = check_origin(
            &headers(Some("http://localhost:9999"), Some("other.example")),
            &cfg,
        );
        match orig {
            Some(v) => std::env::set_var("KB_DEV_ORIGIN_ANY", v),
            None => std::env::remove_var("KB_DEV_ORIGIN_ANY"),
        }
        assert_eq!(refused, Err(GateRefusal::Origin));
    }

    #[test]
    fn non_utf8_origin_is_refused() {
        let mut h = HeaderMap::new();
        h.insert(
            header::ORIGIN,
            header::HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        h.insert(header::HOST, "x".parse().unwrap());
        assert_eq!(check_origin(&h, &cfg()), Err(GateRefusal::Origin));
    }

    #[test]
    fn terminal_intent_table() {
        for v in ["terminal", "Terminal", "TERMINAL", "  terminal  "] {
            let mut h = HeaderMap::new();
            h.insert(TERMINAL_INTENT_HEADER, v.parse().unwrap());
            assert!(terminal_intent(&h), "{v} must pass");
        }
        for v in ["nope", "", "terminal2"] {
            let mut h = HeaderMap::new();
            h.insert(TERMINAL_INTENT_HEADER, v.parse().unwrap());
            assert!(!terminal_intent(&h), "{v} must fail");
        }
        assert!(!terminal_intent(&HeaderMap::new()));
    }
}
