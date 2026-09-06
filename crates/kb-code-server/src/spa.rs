//! Serves `web-code/dist/` (the kb-code reader SPA build) at the daemon's
//! own origin — W4.1. Mirrors kb-server's `routes::spa`
//! (`crates/kb-server/src/routes/spa.rs`) shape: a request whose path has a
//! file extension is treated as a built asset (served from disk with a
//! cache header); everything else falls back to `index.html` so React
//! Router can handle the client-side route. A dotted path that ISN'T a real
//! asset on disk still falls back to the shell when it's under
//! [`is_client_route`]'s one carve-out (`/r/{repo}/*`, whose splat is a real
//! repo source-file path, e.g. `.../src/app.tsx`) — otherwise a hard
//! navigate/refresh on a reader URL 404s instead of letting React Router
//! handle it. Deliberately SMALLER than kb-server's version — kb-code has no
//! artifact subdomains (one daemon, one origin, no per-corpus Host dispatch)
//! and no public permalinks to splice per-page OpenGraph `<meta>` into, so
//! neither of those pieces is ported here. No mtime-guarded shell cache
//! either (kb-server added that under real parent-origin + artifact-
//! permalink request volume — W4.1's single-operator, loopback-only daemon
//! has no equivalent hot path; add one here if that ever changes).
//!
//! Mounted as the router's top-level `.fallback(...)` (`router.rs`), i.e.
//! only for requests that don't match the nested `/api/*` tree — same
//! placement kb-server uses for its own `routes::dispatch::fallback`.
//!
//! # V70-A2 — Content-Security-Policy
//!
//! The v7 critique's MISSING #3: *"CSP + security headers for both SPAs.
//! Zero today. With Markdown, mermaid, imported themes and imported HTML
//! all landing in v7, a CSP is the cheapest structural defence available
//! and belongs in v7.0, not v7.6."*
//!
//! Every response this module serves — the shell AND each built asset —
//! carries [`CSP`]. Directive by directive:
//!
//! * `default-src 'self'` / `script-src 'self'` — no inline script, no
//!   remote script. `web-code/index.html` carries exactly ONE `<script
//!   type="module" src="/src/main.tsx">` tag (Vite rewrites it to the
//!   hashed bundle at build time) and no inline `<script>` at all, so
//!   this needs no `'unsafe-inline'` and no nonce plumbing.
//!   `spa_index_html_has_no_inline_script` pins that.
//! * `style-src 'self' 'unsafe-inline'` — CodeMirror 6 injects its
//!   theme/highlight rules as inline `<style>` at runtime (`EditorView.
//!   theme`/`StyleModule`), and so does the reader's own per-line
//!   decoration layer. This is the one relaxation, and it is a
//!   requirement of a dependency rather than a convenience.
//! * `img-src 'self' data: blob:` — `data:` for the inline icon set,
//!   `blob:` for canvas/screenshot exports.
//! * `font-src 'self'` — v4.0 self-hosts its fonts (no Google Fonts).
//! * `connect-src 'self'` — the SPA only ever talks to its own origin
//!   (`web-code/src/api/client.ts`'s module doc: "Same-origin only").
//! * `frame-ancestors 'none'` — kb-code has no embed story; this is the
//!   clickjacking floor, and the header form is what modern browsers
//!   honour (`X-Frame-Options` is the legacy spelling, sent beside it).
//! * `base-uri 'none'` — a `<base href>` injected by an XSS would
//!   re-point every relative `/api/...` fetch; nothing here needs one.
//!
//! Beside it: `X-Content-Type-Options: nosniff` (the MIME map in
//! `guess_content_type` is deliberately small and falls back to
//! `application/octet-stream`, which a sniffing browser would otherwise
//! second-guess) and `Referrer-Policy: no-referrer` (a reader URL carries
//! repo and file paths; they belong in no outbound `Referer`).
//!
//! Deliberately NOT set here: HSTS (a loopback dev daemon is http, and the
//! prod TLS terminator owns that header) and `Permissions-Policy` (nothing
//! in this SPA touches a gated capability, so an allowlist would be
//! decoration).

use crate::state::SharedState;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Response, StatusCode, Uri},
    response::IntoResponse,
};
use std::path::{Path, PathBuf};

/// The Content-Security-Policy every SPA response carries — see the
/// module doc for the directive-by-directive rationale.
pub const CSP: &str = "default-src 'self'; script-src 'self'; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
     font-src 'self'; connect-src 'self'; frame-ancestors 'none'; \
     base-uri 'none'";

/// Attach [`CSP`] and the three companion headers to a response. Applied
/// to the shell, every asset, and both 404 shapes — a header set that
/// covers "almost every response" is a header set with a hole.
fn with_security_headers(mut resp: Response<Body>) -> Response<Body> {
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    // The legacy spelling of `frame-ancestors 'none'`, for the browsers
    // and scanners that still look for it.
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    resp
}

/// Resolve the SPA dist directory at boot. Order mirrors kb-server's
/// `routes::spa::resolve_spa_dist` exactly (same two-step precedence, same
/// "index.html must actually exist" check), substituting kb-code's own env
/// var + directory name:
///   1. `KB_CODE_SPA_DIST` env var (absolute path to a built dist dir)
///   2. `web-code/dist` relative to the current working directory
///   3. `None` — the daemon serves `/api/*` only; every other path 404s
///      (lets `cargo test`/CI boot the daemon without building the SPA).
pub fn resolve_spa_dist() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KB_CODE_SPA_DIST") {
        let pb = PathBuf::from(p);
        if pb.join("index.html").is_file() {
            return Some(pb);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join("web-code").join("dist");
    if candidate.join("index.html").is_file() {
        Some(candidate)
    } else {
        None
    }
}

/// The fallback handler itself — resolved SPA dist (or a friendly 404 JSON
/// body when the daemon booted without one) → asset-or-shell.
pub async fn serve(State(state): State<SharedState>, uri: Uri) -> Response<Body> {
    with_security_headers(serve_inner(state, uri))
}

/// The pre-V70-A2 body of [`serve`], unchanged — split out so the header
/// set is applied at ONE point and cannot miss a branch.
fn serve_inner(state: SharedState, uri: Uri) -> Response<Body> {
    let Some(spa_dist) = state.spa_dist.as_ref() else {
        return spa_unavailable();
    };
    let raw_path = uri.path();

    // Reject any path containing `..` segments outright — the SPA's real
    // routes never contain one, and `read_asset`'s canonicalize check below
    // already prevents an actual escape, but this short-circuits the
    // obviously-hostile case before touching the filesystem at all (same
    // belt-and-suspenders as kb-server's `routes::spa::serve`).
    if raw_path.split('/').any(|s| s == "..") {
        return not_found();
    }

    let trimmed = raw_path.trim_start_matches('/');
    let candidate = spa_dist.join(trimmed);
    if has_extension(trimmed) {
        match read_asset(spa_dist, &candidate) {
            Some(resp) => resp,
            // A dotted path under `/r/{repo}/*` is a real repo source-file
            // segment the reader is displaying (e.g. `.../src/app.tsx`),
            // not a missing built asset — see `is_client_route`'s doc.
            // Serve the shell so React Router's `/r/:repo/*` route can
            // handle the hard-navigate/refresh; a genuine missing asset
            // anywhere else still 404s.
            None if is_client_route(trimmed) => serve_shell(spa_dist),
            None => not_found(),
        }
    } else {
        serve_shell(spa_dist)
    }
}

/// True for the SPA client routes that legitimately carry a file
/// extension: `/r/{repo}/*` (`web-code/src/app.tsx`'s route table) — its
/// splat is a real repo file path (e.g. `blob/main/src/app.tsx`), so
/// `has_extension` alone can't tell "a built asset the SPA emitted" from "a
/// source file the reader is browsing". Mirrors kb-server's
/// `routes::spa::is_client_route` (`crates/kb-server/src/routes/spa.rs`,
/// which reserves `a/{kb}/*` for the identical reason). kb-code's other
/// client routes (`/`, `/search`, `/session/:sid/diff`) never carry a dot,
/// so they need no carve-out here.
///
/// DCB W2.B (R2) adds the second one: `/~lens/{kb}/by-path/{*path}`
/// (`LensEntryByPath.tsx`) — its splat is a real kb SOURCE-RELATIVE DOC
/// path (e.g. `docs/fixture-doc.html`), so a dotted final segment there is
/// just as expected as it is under `/r/{repo}/*`. The sibling id-addressed
/// ramp (`/~lens/{kb}/{docId}`, `LensEntry.tsx`) needs no carve-out: kb's
/// artifact id (D14) is an opaque hex string with no dot.
fn is_client_route(trimmed: &str) -> bool {
    trimmed.starts_with("r/") || is_lens_by_path_route(trimmed)
}

/// `~lens/<kb-segment>/by-path/<rest>` — `<kb-segment>` is exactly one path
/// component (no `/`), matching the route's own `:kb` param; `<rest>` is
/// whatever `by-path/` leads into, unexamined here (the SPA-serving layer
/// is opaque to path CONTENT the same way `is_client_route`'s `r/` arm is).
fn is_lens_by_path_route(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix("~lens/") else {
        return false;
    };
    let Some((_kb, tail)) = rest.split_once('/') else {
        return false;
    };
    tail.starts_with("by-path/")
}

fn read_asset(root: &Path, candidate: &Path) -> Option<Response<Body>> {
    let resolved = candidate.canonicalize().ok()?;
    let root_canonical = root.canonicalize().ok()?;
    if !resolved.starts_with(&root_canonical) {
        return None;
    }
    let bytes = std::fs::read(&resolved).ok()?;
    let ct = guess_content_type(&resolved);
    let mut resp = (StatusCode::OK, bytes).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    // Vite hashes every `/assets/...` filename, so those are safe to cache
    // forever; everything else (an unhashed top-level file) gets a shorter
    // window. Mirrors kb-server's `routes::spa::read_asset` cache split,
    // minus the `.webmanifest` special case (kb-code ships no PWA manifest).
    let cache = if resolved
        .strip_prefix(&root_canonical)
        .map(|p| p.starts_with("assets"))
        .unwrap_or(false)
    {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=3600"
    };
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Some(resp)
}

fn serve_shell(spa_dist: &Path) -> Response<Body> {
    match std::fs::read(spa_dist.join("index.html")) {
        Ok(bytes) => {
            let mut resp = (StatusCode::OK, bytes).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            resp.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            resp
        }
        Err(_) => not_found(),
    }
}

/// Minimal extension → MIME map — the SPA build only ever emits these five
/// kinds of static asset (JS/CSS chunks, source maps, the favicon, and
/// index.html itself, which never reaches this fn — `serve_shell` sets its
/// content type directly). Falls back to `application/octet-stream` for
/// anything else rather than guessing.
fn guess_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("map") | Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn has_extension(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some()
}

fn not_found() -> Response<Body> {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn spa_unavailable() -> Response<Body> {
    let body = serde_json::json!({
        "type": "urn:kb-code:errors:spa-unavailable",
        "title": "SPA not built",
        "status": 404,
        "detail": "The daemon was started without a SPA dist. Build with \
                    `cd web-code && npm run build` and restart, or set KB_CODE_SPA_DIST.",
    });
    let mut resp = (StatusCode::NOT_FOUND, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both tests below mutate the process-global `KB_CODE_SPA_DIST` env
    // var — serialised the same way `kb_core::embed_ipc`'s own env-mutating
    // tests are (see that module's `ENV_LOCK`), so parallel `cargo test`
    // execution can't race one test's `set_var` against another's read.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// V70-A2 — the CSP's `script-src 'self'` (no `'unsafe-inline'`, no
    /// nonce) is only sound while the shell has NO inline script. Pinned
    /// against the real source file, so adding one breaks the build rather
    /// than silently breaking the SPA at runtime under the policy.
    #[test]
    fn spa_index_html_has_no_inline_script() {
        let index =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web-code/index.html");
        let html = std::fs::read_to_string(&index)
            .unwrap_or_else(|e| panic!("read {}: {e}", index.display()));
        // Every `<script` in the shell must carry a `src=` (an external
        // module Vite rewrites to a hashed bundle), never a body.
        for (i, chunk) in html.split("<script").enumerate().skip(1) {
            let tag = chunk.split('>').next().unwrap_or("");
            assert!(
                tag.contains("src="),
                "web-code/index.html script #{i} is INLINE; the CSP's \
                 `script-src 'self'` would block it: <script{tag}>"
            );
        }
        assert!(
            !html.contains("<base "),
            "a <base href> would defeat base-uri 'none'"
        );
    }

    #[test]
    fn the_csp_carries_every_directive_the_reader_needs() {
        // CodeMirror injects inline <style>; nothing injects inline script.
        assert!(CSP.contains("style-src 'self' 'unsafe-inline'"));
        assert!(CSP.contains("script-src 'self';"));
        assert!(!CSP.contains("script-src 'self' 'unsafe-inline'"));
        assert!(CSP.contains("frame-ancestors 'none'"));
        assert!(CSP.contains("base-uri 'none'"));
        assert!(CSP.contains("connect-src 'self'"));
        assert!(CSP.contains("img-src 'self' data: blob:"));
        assert!(CSP.contains("font-src 'self'"));
    }

    #[test]
    fn every_spa_response_shape_carries_the_security_headers() {
        // Applied at ONE point (`serve`), so this holds for the shell,
        // every asset and both 404 bodies by construction — assert it on
        // the shape that has the least machinery behind it.
        let resp = with_security_headers(not_found());
        assert_eq!(
            resp.headers().get(header::CONTENT_SECURITY_POLICY).unwrap(),
            CSP
        );
        assert_eq!(
            resp.headers().get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        assert_eq!(
            resp.headers().get(header::REFERRER_POLICY).unwrap(),
            "no-referrer"
        );
        assert_eq!(resp.headers().get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
    }

    #[test]
    fn resolve_spa_dist_reads_the_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("index.html"), b"<html></html>").unwrap();
        std::env::set_var("KB_CODE_SPA_DIST", tmp.path());
        let resolved = resolve_spa_dist();
        std::env::remove_var("KB_CODE_SPA_DIST");
        assert_eq!(resolved.as_deref(), Some(tmp.path()));
    }

    #[test]
    fn resolve_spa_dist_env_override_needs_a_real_index_html() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        // No index.html written — the override must be ignored, not trusted
        // blindly, so a stale/misconfigured env var degrades to "no SPA"
        // rather than serving a directory with nothing in it.
        std::env::set_var("KB_CODE_SPA_DIST", tmp.path());
        let resolved = resolve_spa_dist();
        std::env::remove_var("KB_CODE_SPA_DIST");
        assert_ne!(resolved.as_deref(), Some(tmp.path()));
    }

    #[test]
    fn has_extension_distinguishes_assets_from_client_routes() {
        assert!(has_extension("assets/main-abc123.js"));
        assert!(has_extension("logomark.svg"));
        assert!(!has_extension(""));
        // A client-routed path segment (a repo name, a directory) has no
        // extension of its own — even though it sits underneath a real
        // source file that WOULD have one (`has_extension`'s job is "does
        // THIS path look like a built asset", not "does this path lead to
        // a file somewhere" — the reader's own repo/path segments are
        // opaque to the SPA-serving layer either way).
        assert!(!has_extension("repo/main/src"));
    }

    #[test]
    fn is_client_route_covers_only_the_r_splat() {
        assert!(is_client_route("r/kb/blob/main/src/app.tsx"));
        assert!(is_client_route("r/kb"));
        assert!(!is_client_route("assets/main-abc123.js"));
        assert!(!is_client_route("search"));
        assert!(!is_client_route(""));
    }

    /// DCB W2.B (R2) — the path-addressed lens entry ramp's splat is a real
    /// dotted doc path, needing the SAME carve-out `r/{repo}/*` gets.
    #[test]
    fn is_client_route_also_covers_the_lens_by_path_splat() {
        assert!(is_client_route(
            "~lens/fixture-kb/by-path/docs/fixture-doc.html"
        ));
        assert!(is_client_route("~lens/fixture-kb/by-path/a.html"));
        // The id-addressed ramp never carries a dot (D14's opaque hex id),
        // so it needs no carve-out — but confirm the fn doesn't wrongly
        // claim it either way.
        assert!(!is_client_route("~lens/fixture-kb/9f8b7182d433"));
        // A `~lens` path missing the `by-path/` marker (malformed/foreign)
        // must not slip through.
        assert!(!is_client_route("~lens/fixture-kb/nope/docs/a.html"));
        assert!(!is_client_route("~lens/fixture-kb"));
        assert!(!is_client_route("~lens"));
    }

    #[test]
    fn guess_content_type_covers_the_build_output_set() {
        assert_eq!(
            guess_content_type(Path::new("a.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            guess_content_type(Path::new("a.css")),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            guess_content_type(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }

    #[test]
    fn read_asset_refuses_to_escape_the_dist_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("index.html"), b"<html></html>").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.js");
        std::fs::write(&secret, b"leak").unwrap();
        // `secret` resolves to a real file, just not one under `tmp` (the
        // dist root) — `read_asset`'s canonicalize + `starts_with` guard
        // must refuse it regardless of how the candidate path was built (a
        // real `..`-based traversal is already rejected earlier, in
        // `serve`, before `read_asset` is ever called — this exercises the
        // guard directly).
        assert!(read_asset(tmp.path(), &secret).is_none());
    }
}
