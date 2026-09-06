//! `prompt` — W2.11: the generation prompt becomes first-class content.
//!
//! ```text
//! GET /api/kb/{kb}/artifacts/{id}/prompt
//!     → { id, prompt: string | null, size_bytes, stripped }
//! ```
//!
//! The stored `prompt` column is the byte-for-byte inner HTML of the
//! artifact's `<template id="kb-prompt">` (CLAUDE.md invariant #5),
//! index-time-capped at `kb_core::parser::PROMPT_MAX_BYTES` (8 KiB). This
//! route is the only place that ever projects it onto the wire.
//!
//! **LOCAL-RENDER ONLY** on strip-configured corpora — this route applies
//! the *exact* gate `GET /api/kb/{kb}/artifact/{id}` (`routes::docs::
//! artifact_bytes`) already applies to full artifact bytes: auth and scrub
//! are different threat models, so an operator who opted into `[kb.*.
//! outbound] strip_kb_prompt = true` expects the prompt withheld from any
//! non-loopback pull, not just browser fetches. Unlike `artifact_bytes`
//! (which 404s a scrub-refused fetch is never issued — it just serves
//! scrubbed bytes), a stripped prompt is reported as a plain **200** with
//! `prompt: null, stripped: true` — the corpus has no prompt to hide, it's
//! just choosing not to show this reader one, and that's worth saying
//! honestly rather than behind a generic 404. When the kb also has
//! `[outbound] redactions` configured, those regex rules run over the
//! prompt text on a non-loopback response too (the stored column never
//! passes through them at index time — only the full-document scrub path
//! does). Loopback always serves verbatim.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, Response},
    response::IntoResponse,
    Json,
};
use kb_core::scrub::OutboundCache;
use serde::Serialize;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct PromptResponse {
    pub id: String,
    /// `None` both when the artifact never had a `<template id="kb-prompt">`
    /// and when `stripped` is true — the caller distinguishes the two via
    /// `stripped`, never by re-deriving absence from `size_bytes`.
    pub prompt: Option<String>,
    /// The stored (index-time-capped) byte length. `0` when `prompt` is
    /// `None` for either reason above — never the true size on a stripped
    /// response (that would leak a signal the strip is meant to withhold).
    pub size_bytes: u32,
    /// `true` when this corpus's `[outbound] strip_kb_prompt` withheld the
    /// prompt from this (non-loopback) reader. Never conflated with a 404.
    pub stripped: bool,
}

/// The strip-gate decision, factored out of `get` so the header/peer
/// combinations pin without building a full request/response cycle — same
/// TCP-peer-first-then-XFF rule `crate::scrub::looks_non_loopback` applies,
/// exercised the same way `scrub.rs`'s own tests do. Mirrors the
/// `artifact_bytes` gate (routes/docs.rs) exactly: auth and scrub are
/// different threat models, so a strip-configured corpus withholds the
/// prompt from any non-loopback pull, not just browser fetches.
fn should_withhold(
    outbound: Option<&OutboundCache>,
    peer: Option<IpAddr>,
    headers: &HeaderMap,
    trusted_proxies: &[IpAddr],
) -> bool {
    outbound.is_some_and(|c| c.strip_kb_prompt)
        && crate::scrub::looks_non_loopback(peer, headers, trusted_proxies)
}

/// `GET /api/kb/{kb}/artifacts/{id}/prompt`.
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Existence check — this is what turns an unknown/malformed id into a
    // 404; `prompt_by_id` alone can't distinguish "no such doc" from "doc
    // exists, no prompt" (see its doc comment), so it deliberately isn't
    // asked to.
    match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => return error_to_problem_json(&kb_core::Error::NotFound(format!("doc {id}"))),
        Err(e) => return error_to_problem_json(&e),
    }

    if should_withhold(
        ctx.outbound.as_deref(),
        Some(peer.ip()),
        &headers,
        &state.origin.trusted_proxies,
    ) {
        return Json(PromptResponse {
            id,
            prompt: None,
            size_bytes: 0,
            stripped: true,
        })
        .into_response();
    }

    let (prompt, size_bytes) = match ctx.storage.prompt_by_id(id.clone()).await {
        Ok(Some((text, size))) => (Some(text), size),
        Ok(None) => (None, 0),
        Err(e) => return error_to_problem_json(&e),
    };
    // Redactions apply to a non-loopback response even though
    // `strip_kb_prompt` is false here (the branch above already returned
    // for the strip case) — `scrub`'s strip stage is then a guaranteed
    // no-op (the raw prompt text never contains the `<template
    // id="kb-prompt">` wrapper itself), so this reduces to exactly the
    // regex-redaction pass.
    let non_loopback =
        crate::scrub::looks_non_loopback(Some(peer.ip()), &headers, &state.origin.trusted_proxies);
    let prompt = match (&ctx.outbound, prompt) {
        (Some(cache), Some(text)) if non_loopback => Some(crate::scrub::scrub(&text, cache)),
        (_, text) => text,
    };

    Json(PromptResponse {
        id,
        prompt,
        size_bytes,
        stripped: false,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn cache(strip: bool) -> OutboundCache {
        OutboundCache {
            strip_kb_prompt: strip,
            rules: Vec::new(),
        }
    }

    fn loopback() -> Option<IpAddr> {
        Some("127.0.0.1".parse().unwrap())
    }
    fn remote() -> Option<IpAddr> {
        Some("8.8.8.8".parse().unwrap())
    }

    // invariant:5 kb-prompt — strip-configured corpus withholds the prompt
    // from a genuinely non-loopback reader.
    #[test]
    fn withholds_when_strip_configured_and_client_is_remote() {
        let h = HeaderMap::new();
        assert!(should_withhold(Some(&cache(true)), remote(), &h, &[]));
    }

    #[test]
    fn serves_when_strip_configured_but_client_is_loopback() {
        let h = HeaderMap::new();
        assert!(!should_withhold(Some(&cache(true)), loopback(), &h, &[]));
    }

    #[test]
    fn serves_when_client_is_remote_but_strip_not_configured() {
        let h = HeaderMap::new();
        assert!(!should_withhold(Some(&cache(false)), remote(), &h, &[]));
        assert!(!should_withhold(None, remote(), &h, &[]));
    }

    #[test]
    fn withholds_no_peer_fails_closed() {
        // Mirrors `looks_non_loopback`'s own contract: a missing peer
        // never silently downgrades to loopback.
        let h = HeaderMap::new();
        assert!(should_withhold(Some(&cache(true)), None, &h, &[]));
    }

    #[test]
    fn withholds_spoofed_leftmost_loopback_xff_still_scrubs() {
        // A forged `X-Forwarded-For: 127.0.0.1` from a real remote peer
        // must not suppress the withhold — same right-to-left XFF rule.
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("127.0.0.1, 8.8.8.8"),
        );
        assert!(should_withhold(Some(&cache(true)), loopback(), &h, &[]));
    }

    #[test]
    fn serves_trusted_proxy_hop_with_loopback_client_behind_it() {
        let trusted: Vec<IpAddr> = vec!["10.0.0.1".parse().unwrap()];
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("127.0.0.1, 10.0.0.1"),
        );
        assert!(!should_withhold(
            Some(&cache(true)),
            loopback(),
            &h,
            &trusted
        ));
    }
}
