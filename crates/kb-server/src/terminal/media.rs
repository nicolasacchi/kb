//! `GET /api/terms/media/{sha256}` — the stored terminal pictures.
//!
//! The tap decodes each kitty picture once and stores it under its
//! sha256; the browser draws it over the placeholder cells and fetches
//! the bytes by hash through this route. kb-server holds no pictures:
//! it asks the runner (`media` op over the unix socket) and relays the
//! bytes, so no path is ever built from the request here.
//!
//! **The gate is a GET variant.** A same-origin `fetch()` GET carries no
//! `Origin` header, so the REST gate's "absent Origin fails closed"
//! rule cannot apply unchanged. This route instead requires, in order:
//! the tier enabled (else a bodiless 404), `X-Kb-Intent: terminal` (the
//! SPA's `fetch` sets it; an `<img src>` or a cross-site form cannot),
//! an `Origin` that is exact when present, `Sec-Fetch-Site` of
//! `same-origin` or `none` when present, and a non-token caller class.
//! The loopback bypass does not apply. Intent is the part a cross-site
//! page cannot forge; the rest narrows what a browser may send.
//!
//! Bytes are served only if their magic says PNG, JPEG, GIF or WebP
//! (the content type comes from that sniff, never from a stored or
//! client-supplied mime); anything else, SVG included, is a 415.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Extension;

use crate::middleware::Identity;
use crate::state::KbHandles;
use crate::terminal::gate::{
    caller_class_admitted, check_origin, disabled, terminal_intent, GateRefusal, Problem,
    ERR_TERMINAL_BAD_REPLY, ERR_TERMINAL_BAD_REQUEST, ERR_TERMINAL_NOT_FOUND,
    ERR_TERMINAL_ORIGIN_REFUSED, ERR_TERMINAL_PAYLOAD_TOO_LARGE, ERR_TERMINAL_RUNNER_UNAVAILABLE,
};
use crate::terminal::runner_client::{MediaReply, RunnerError, UnixRunner};

/// `urn:kb:errors:unsupported-media` — stored bytes whose magic is not
/// a served picture type.
const ERR_UNSUPPORTED_MEDIA: &str = "urn:kb:errors:unsupported-media";

/// `Sec-Fetch-Site` header name (a forbidden header: scripts cannot set it).
const SEC_FETCH_SITE: &str = "sec-fetch-site";

/// True iff `s` is exactly 64 lowercase hex characters.
pub(crate) fn is_valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The served content type for `bytes`, from magic bytes only.
/// `None` for anything that is not PNG, JPEG, GIF or WebP.
pub(crate) fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The media GET gate (see the module docs). `Ok` means the handler
/// may proceed to the runner.
async fn gate_media(
    state: &KbHandles,
    headers: &HeaderMap,
    identity: &Identity,
) -> Result<(), Response<Body>> {
    let terminal = state.config.read().await.terminal.clone();
    if !terminal.enabled() {
        return Err(disabled());
    }
    if !terminal_intent(headers) {
        return Err(Problem::response(
            GateRefusal::Intent.urn(),
            StatusCode::FORBIDDEN,
            "Forbidden",
            "terminal media requires `X-Kb-Intent: terminal`".to_string(),
        ));
    }
    // Absent Origin is the normal same-origin GET; a present one must be
    // exact (never the artifact-suffix arm, never `KB_DEV_ORIGIN_ANY`).
    if headers.contains_key(header::ORIGIN) {
        check_origin(headers, &state.origin).map_err(Problem::refusal)?;
    }
    if let Some(site) = headers.get(SEC_FETCH_SITE) {
        let ok = site
            .to_str()
            .is_ok_and(|v| v == "same-origin" || v == "none");
        if !ok {
            return Err(Problem::response(
                ERR_TERMINAL_ORIGIN_REFUSED,
                StatusCode::FORBIDDEN,
                "Forbidden",
                "terminal media is same-origin only".to_string(),
            ));
        }
    }
    if !caller_class_admitted(identity.source, terminal.refuse_token_callers()) {
        return Err(Problem::refusal(GateRefusal::Token));
    }
    Ok(())
}

fn problem(urn: &'static str, status: StatusCode, detail: impl Into<String>) -> Response<Body> {
    let mut resp = Problem::response(
        urn,
        status,
        status.canonical_reason().unwrap_or("Error"),
        detail.into(),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

/// Map a runner `err` code (dash or underscore spelling) to a status. The
/// runner's message is fixed text (never a path or argv); an empty one
/// falls back to the route's own wording.
fn runner_err(code: &str, message: &str) -> Response<Body> {
    let detail = |fallback: &str| -> String {
        if message.trim().is_empty() {
            fallback.to_string()
        } else {
            message.to_string()
        }
    };
    match code {
        "not-found" | "not_found" => problem(
            ERR_TERMINAL_NOT_FOUND,
            StatusCode::NOT_FOUND,
            detail("no such picture"),
        ),
        "bad-request" | "bad_request" | "invalid-id" => problem(
            ERR_TERMINAL_BAD_REQUEST,
            StatusCode::BAD_REQUEST,
            detail("the runner refused the media key"),
        ),
        "too-large" | "too_large" => problem(
            ERR_TERMINAL_PAYLOAD_TOO_LARGE,
            StatusCode::PAYLOAD_TOO_LARGE,
            detail("stored picture is too large"),
        ),
        _ => problem(
            ERR_TERMINAL_BAD_REPLY,
            StatusCode::BAD_GATEWAY,
            detail("terminal runner returned an unexpected error"),
        ),
    }
}

/// The response for sniffed picture bytes: the sniffed type, nosniff,
/// a year-long private immutable cache (the URL is content-addressed),
/// a sandboxing CSP, inline disposition.
fn picture(content_type: &'static str, bytes: Vec<u8>) -> Response<Body> {
    let mut resp = Response::new(Body::from(bytes));
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    resp
}

/// `GET /api/terms/media/{sha256}`.
pub async fn serve(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<Identity>,
    headers: HeaderMap,
    Path(sha256): Path<String>,
) -> Response<Body> {
    if let Err(e) = gate_media(&state, &headers, &identity).await {
        return e;
    }
    // The hash is the only request value that goes anywhere; it is
    // validated before the runner is even built.
    if !is_valid_sha256(&sha256) {
        return problem(
            ERR_TERMINAL_BAD_REQUEST,
            StatusCode::BAD_REQUEST,
            "media key must be 64 lowercase hex characters",
        );
    }
    let runner = {
        let cfg = state.config.read().await;
        let terminal = &cfg.terminal;
        UnixRunner::new(
            terminal.resolved_runner_socket(),
            Duration::from_secs(terminal.runner_timeout_secs()),
        )
    };
    match runner.media(&sha256).await {
        Ok(MediaReply::Bytes(bytes)) => match sniff_image(&bytes) {
            Some(ct) => picture(ct, bytes),
            None => problem(
                ERR_UNSUPPORTED_MEDIA,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "stored bytes are not a served picture type",
            ),
        },
        Ok(MediaReply::Err { code, message }) => runner_err(&code, &message),
        Err(RunnerError::Unavailable | RunnerError::Timeout) => problem(
            ERR_TERMINAL_RUNNER_UNAVAILABLE,
            StatusCode::BAD_GATEWAY,
            "terminal runner unavailable",
        ),
        Err(RunnerError::TooLarge | RunnerError::BadReply) => problem(
            ERR_TERMINAL_BAD_REPLY,
            StatusCode::BAD_GATEWAY,
            "terminal runner returned an unreadable reply",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_grammar_is_lowercase_hex64() {
        let ok = "0123456789abcdef".repeat(4);
        assert!(is_valid_sha256(&ok));
        assert!(!is_valid_sha256(&ok.to_uppercase()));
        assert!(!is_valid_sha256(&ok[..63]));
        assert!(!is_valid_sha256(&format!("{ok}0")));
        assert!(!is_valid_sha256(""));
        assert!(!is_valid_sha256(&format!("{}g", &ok[..63])));
    }

    #[test]
    fn sniff_accepts_the_four_picture_types_and_refuses_everything_else() {
        assert_eq!(
            sniff_image(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0]),
            Some("image/png")
        );
        assert_eq!(sniff_image(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF87a...."), Some("image/gif"));
        assert_eq!(sniff_image(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WAVEfmt "), None);
        assert_eq!(
            sniff_image(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            None
        );
        assert_eq!(sniff_image(b"<?xml version=\"1.0\"?><svg/>"), None);
        assert_eq!(sniff_image(b"GIF90a"), None);
        assert_eq!(sniff_image(b""), None);
    }
}
