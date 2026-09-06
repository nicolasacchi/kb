//! Outbound scrubbing — serve-path gate.
//!
//! The scrub *transform* (strip `<template id="kb-prompt">` + ordered
//! regex redactions) lives in [`kb_core::scrub`] so the static-export
//! engine reuses the exact same logic. It is re-exported here as
//! [`scrub`] for the serve-path callers (`routes/artifact.rs`,
//! `routes/docs.rs`).
//!
//! What stays here is the *decision* of when to scrub: `looks_non_loopback`
//! gates the serve-path on the request's genuine client looking
//! non-loopback (a tunnel / reverse proxy in front). It depends on the
//! HTTP middleware's loopback rule, so it cannot move to kb-core.

pub use kb_core::scrub::scrub;

use axum::http::HeaderMap;
use std::net::IpAddr;

/// Returns `true` when the request's genuine client looks non-loopback —
/// the signal that gates outbound scrubbing.
///
/// Same rule as auth/rate-limit (CLAUDE.md invariant 6): the immediate
/// TCP peer must be a trusted hop before `X-Forwarded-For` is consulted,
/// and the XFF chain is walked RIGHT-TO-LEFT skipping trusted hops. A
/// direct connection from a non-loopback IP — the self-host case where
/// the daemon binds `0.0.0.0` with no reverse proxy in front — counts
/// as non-loopback regardless of headers, so the scrub layer engages.
///
/// `peer = None` fails CLOSED (treats the request as non-loopback, so
/// scrubbing runs). This mirrors [`crate::middleware::is_loopback_origin`]
/// — better to scrub a local-loopback test that forgot to wire
/// `ConnectInfo` than to leak a `<template id="kb-prompt">` to a real
/// remote client over a misconfigured route.
pub fn looks_non_loopback(
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    trusted_proxies: &[IpAddr],
) -> bool {
    !crate::middleware::is_loopback_origin(peer, headers, trusted_proxies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn loopback() -> Option<IpAddr> {
        Some("127.0.0.1".parse().unwrap())
    }
    fn remote() -> Option<IpAddr> {
        Some("8.8.8.8".parse().unwrap())
    }

    #[test]
    fn looks_non_loopback_loopback_peer_no_xff_is_local() {
        let h = HeaderMap::new();
        assert!(!looks_non_loopback(loopback(), &h, &[]));
    }

    #[test]
    fn looks_non_loopback_loopback_xff_from_loopback_peer_is_local() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("127.0.0.1"));
        assert!(!looks_non_loopback(loopback(), &h, &[]));
        h.insert("x-forwarded-for", HeaderValue::from_static("::1"));
        assert!(!looks_non_loopback(loopback(), &h, &[]));
    }

    #[test]
    fn looks_non_loopback_external_xff_from_loopback_peer_triggers_scrub() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("8.8.8.8"));
        assert!(looks_non_loopback(loopback(), &h, &[]));
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("8.8.8.8, 10.0.0.1"),
        );
        assert!(looks_non_loopback(loopback(), &h, &[]));
    }

    #[test]
    fn looks_non_loopback_unparseable_xff_from_loopback_is_local() {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert!(!looks_non_loopback(loopback(), &h, &[]));
    }

    #[test]
    fn looks_non_loopback_spoofed_leftmost_loopback_still_scrubs() {
        // v0.7.1 C1: a client forging `X-Forwarded-For: 127.0.0.1` while
        // a real proxy appends its true IP must NOT suppress the scrub.
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("127.0.0.1, 8.8.8.8"),
        );
        assert!(looks_non_loopback(loopback(), &h, &[]));
    }

    #[test]
    fn looks_non_loopback_skips_trusted_proxy_hops() {
        let trusted: Vec<IpAddr> = vec!["10.0.0.1".parse().unwrap()];
        let mut h = HeaderMap::new();
        // Real client behind a trusted proxy → still scrubs.
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("8.8.8.8, 10.0.0.1"),
        );
        assert!(looks_non_loopback(loopback(), &h, &trusted));
        // Whole chain trusted/loopback → local, no scrub.
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("127.0.0.1, 10.0.0.1"),
        );
        assert!(!looks_non_loopback(loopback(), &h, &trusted));
    }

    // --- TCP peer rule (matches CLAUDE.md invariant 6) ---------------------

    #[test]
    fn looks_non_loopback_remote_peer_no_xff_triggers_scrub() {
        // The hole the deep-review's S2 finding closed: a daemon bound
        // 0.0.0.0 with no reverse proxy in front, talked to directly by a
        // remote browser, sends NO X-Forwarded-For. Before this fix
        // `looks_non_loopback` returned `false` and the kb-prompt + PII
        // regex layer never ran.
        let h = HeaderMap::new();
        assert!(looks_non_loopback(remote(), &h, &[]));
    }

    #[test]
    fn looks_non_loopback_remote_peer_with_forged_xff_still_scrubs() {
        // A direct remote client can't suppress scrub by sending
        // `X-Forwarded-For: 127.0.0.1` — the peer rule kicks in first.
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", HeaderValue::from_static("127.0.0.1"));
        assert!(looks_non_loopback(remote(), &h, &[]));
    }

    #[test]
    fn looks_non_loopback_no_peer_fails_closed() {
        // Mirrors `request_is_loopback`: a missing ConnectInfo (would only
        // happen if a future refactor served the router bare) must NOT
        // silently bypass scrub.
        let h = HeaderMap::new();
        assert!(looks_non_loopback(None, &h, &[]));
    }
}
