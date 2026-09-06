//! DCB W1.C — the doc-lens lane's CORS layers.
//!
//! These layers mount on the doc-lens SUB-ROUTERS ONLY (see `router.rs`).
//! kb-server layers its own `CorsLayer` over one UNIFORM `/api` nest; THIS
//! crate's `/api` is not uniform, and copying that placement would put
//! `access-control-allow-origin` on the loopback-only transcripts lane
//! (`/api/search/transcripts`, `/api/transcripts/status`, `/api/session-diff`
//! are all GETs) and on `/api/file` — i.e. it would hand any page served on
//! any localhost port the full contents of every configured repo.
//! `router.rs`'s `cors_layer_route_set_is_pinned` probe exists to keep that
//! from happening by accident later.

use axum::http::{header, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// True for any loopback web origin (kb-server's SW3 predicate,
/// `kb_server::middleware::is_loopback_web_origin` — imported, never
/// re-derived) OR an EXACT match in `[doclens] origins`. Comparison is
/// ASCII-case-insensitive on the whole origin string after trimming a
/// trailing `/`. Never a suffix/prefix/wildcard test: `https://evil-kb.example.com`
/// and `https://kb.example.com.evil.test` must both fail against
/// `https://kb.example.com`.
///
/// `extra` is expected to already be `DoclensSection::normalized_origins()`
/// output (lowercased, trailing-`/`-trimmed, malformed entries dropped); this
/// fn normalises the INBOUND origin the same way so the two sides can never
/// disagree about a trailing slash or letter case.
pub fn origin_allowed(origin: &str, extra: &[String]) -> bool {
    if kb_server::middleware::is_loopback_web_origin(origin) {
        return true;
    }
    match crate::config::normalize_origin(origin) {
        Some(norm) => extra.contains(&norm),
        None => false,
    }
}

/// GET-only layer for the two read routes.
pub fn read_cors(extra: Vec<String>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |o, _| {
            o.to_str()
                .map(|s| origin_allowed(s, &extra))
                .unwrap_or(false)
        }))
        .allow_methods([Method::GET])
        // D-A: same-site (*.example.com) credentialed simple GET — the Authelia
        // session cookie must ride, so the response needs exact-origin ACAO
        // (which `AllowOrigin::predicate` emits, plus `Vary: Origin`) and
        // Allow-Credentials. This adds no local authority: a loopback page
        // already reads this daemon under invariant #4's loopback bypass
        // without sending any credential at all.
        .allow_credentials(true)
        .max_age(std::time::Duration::from_secs(600))
}

/// PUT/DELETE + preflight, for `/api/doc-lens/pin` ONLY.
pub fn pin_cors(extra: Vec<String>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |o, _| {
            o.to_str()
                .map(|s| origin_allowed(s, &extra))
                .unwrap_or(false)
        }))
        .allow_methods([Method::PUT, Method::DELETE])
        // A JSON body is not a CORS-safelisted Content-Type, so the PUT needs
        // a preflight and this header. `authorization` is DELIBERATELY NOT
        // allowed: in prod the bearer is injected at the edge (traefik) and
        // locally the loopback bypass applies, so a browser page never holds
        // a token — allowing the header would invite one to.
        .allow_headers([header::CONTENT_TYPE])
        .allow_credentials(true)
        .max_age(std::time::Duration::from_secs(600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DoclensSection;

    fn allowlist(entries: &[&str]) -> Vec<String> {
        DoclensSection {
            origins: entries.iter().map(|s| s.to_string()).collect(),
            ..DoclensSection::default()
        }
        .normalized_origins()
    }

    #[test]
    fn origin_allowed_accepts_loopback_and_exact_allowlist_only() {
        let extra = allowlist(&["https://kb.example.com"]);
        // loopback, any port, any scheme — kb-server's own SW3 predicate.
        assert!(origin_allowed("http://localhost:3000", &[]));
        assert!(origin_allowed("http://127.0.0.1:4747", &[]));
        assert!(origin_allowed("https://localhost", &[]));
        assert!(origin_allowed("http://[::1]:4001", &[]));
        // exact allowlist entry, case-insensitively, trailing slash tolerated
        assert!(origin_allowed("https://kb.example.com", &extra));
        assert!(origin_allowed("https://KB.EXAMPLE.COM", &extra));
        assert!(origin_allowed("https://kb.example.com/", &extra));
        // not allowlisted
        assert!(!origin_allowed("https://kb.example.com", &[]));
        assert!(
            !origin_allowed("http://kb.example.com", &extra),
            "scheme matters"
        );
        assert!(
            !origin_allowed("https://kb.example.com:8443", &extra),
            "port matters"
        );
        assert!(!origin_allowed("null", &extra));
        assert!(!origin_allowed("", &extra));
    }

    #[test]
    fn origin_allowed_rejects_suffix_prefix_and_wildcard_lookalikes() {
        let extra = allowlist(&["https://kb.example.com"]);
        for evil in [
            "https://evil-kb.example.com",
            "https://kb.example.com.evil.test",
            "https://kb.example.comevil.test",
            "https://example.com",
            "https://kb.example.com.",
            "https://kb.example.com@evil.test",
        ] {
            assert!(!origin_allowed(evil, &extra), "must reject {evil}");
        }
    }

    #[test]
    fn normalized_origins_drops_malformed_entries_with_a_warning() {
        let got = allowlist(&[
            "https://kb.example.com/",
            "HTTPS://Kb.Example.Com:8443",
            // every one of these is malformed and must be dropped
            "kb.example.com",
            "https://*.example.com",
            "https://kb.example.com/path",
            "ftp://kb.example.com",
            "https://",
            "",
        ]);
        assert_eq!(
            got,
            vec![
                "https://kb.example.com".to_string(),
                "https://kb.example.com:8443".to_string()
            ]
        );
    }

    #[test]
    fn an_empty_allowlist_is_loopback_only() {
        assert!(origin_allowed("http://localhost:5173", &[]));
        assert!(!origin_allowed("https://kb.example.com", &[]));
        assert!(!origin_allowed("https://example.com", &[]));
    }
}
