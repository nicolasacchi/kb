//! V70-A2 — local-daemon hardening.
//!
//! The v7 security critique (`docs/research/kb-code-v7-evidence/critique/
//! security-mutation.md`) records one frame this module exists to fix:
//!
//! > **loopback is not an authorization boundary — it is only "same
//! > machine".**
//!
//! Every "loopback-only" mutation in `router.rs` is gated on the PEER IP
//! (`kb_server::middleware::request_is_loopback`, invariant #4). On the
//! operator's dev box that peer is also every web page in the operator's
//! browser, and `doclens/cors.rs` already says so in as many words: *"a
//! loopback page already reads this daemon under invariant #4's loopback
//! bypass without sending any credential at all."* Nothing in this crate
//! checked `Origin`, `Host` or any CSRF token before this unit (SEC-02),
//! working-tree reads were path-LEXICAL only (SEC-13), no mutation was
//! auditable (SEC-20), and merge-check wrote objects straight into the
//! browsed repo (SEC-15).
//!
//! Four guards, each in its own submodule:
//!
//! - [`origin`] — the Origin + Host allowlist middleware and the
//!   `X-Kbc-Request: 1` mutation-header middleware. Together they are the
//!   anti-DNS-rebinding and anti-drive-by-CSRF pair; the module doc there
//!   carries the full admission table and the "why no separate CSRF
//!   token" argument.
//! - [`paths`] — canonicalising path containment for every working-tree
//!   read (invariant #27's own discipline, applied at read time).
//! - [`secrets`] — the server-enforced secret denylist (a typed refusal,
//!   never the bytes) plus the non-blocking `redaction_hint` content
//!   sniff.
//! - [`audit`] — the append-only `mutations` ledger (V0027) and the
//!   middleware that writes one row per mutating `/api` request.
//!
//! # Typed refusals
//!
//! Every guard refuses with `application/problem+json` and a stable `type`
//! URN, so a client can branch on the machine code rather than parse
//! prose. The vocabulary this unit adds:
//!
//! | URN | Guard |
//! |---|---|
//! | `urn:kb:errors:origin-refused` | [`origin`] — `Origin`/`Host` not in the allowlist |
//! | `urn:kb:errors:missing-request-header` | [`origin`] — mutating request without `X-Kbc-Request: 1` |
//! | `urn:kb:errors:path-outside-repo` | [`paths`] — a joined path that canonicalises outside the repo root |
//! | `urn:kb:errors:redacted-by-policy` | [`secrets`] — a denylisted path, with the matched PATTERN named |
//!
//! These use the `urn:kb:` prefix (kb-server's own vocabulary, e.g.
//! `urn:kb:errors:not-owner`) rather than this crate's one-off
//! `urn:kb-code:errors:spa-unavailable`: the guards are the SIBLING-wide
//! posture the design brief names, and the sibling daemons should not
//! disagree on the spelling of "you were refused for a security reason".

pub mod audit;
pub mod origin;
pub mod paths;
pub mod secrets;

use axum::body::Body;
use axum::http::Request;

/// The nest prefix every guard in this module is layered under
/// (`router::build_router`'s `.nest("/api", api_all)`).
pub const API_PREFIX: &str = "/api";

/// The request's path as the CALLER wrote it — `/api/checkout`, not
/// `/checkout`.
///
/// This matters because these guards are layered INSIDE `Router::nest`,
/// and axum strips the matched prefix from `req.uri()` before the nested
/// router (and its middleware) ever see it. A path-keyed decision made on
/// the stripped form is silently wrong: `MUTATION_HEADER_EXEMPT_PREFIXES`
/// would never match `/api/doc-lens/pin`, and every `mutations` row would
/// record a route a caller cannot address. Caught by
/// `tests/security/audit_route.rs`'s route assertion — recorded here so
/// the next path-keyed guard does not re-learn it.
///
/// Prefers axum's own `OriginalUri` (inserted by `nest` under its
/// default-on `original-uri` feature) and falls back to re-attaching
/// [`API_PREFIX`], so the answer is right whether or not that feature is
/// ever turned off.
pub fn full_path(req: &Request<Body>) -> String {
    if let Some(axum::extract::OriginalUri(uri)) =
        req.extensions().get::<axum::extract::OriginalUri>()
    {
        return uri.path().to_string();
    }
    let p = req.uri().path();
    if p.starts_with(API_PREFIX) {
        p.to_string()
    } else {
        format!("{API_PREFIX}{p}")
    }
}
