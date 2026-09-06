//! SPA serving — serves `web/dist/` static files at the parent origin.
//!
//! Two responsibilities:
//! 1. **Asset requests** (`/assets/...`, `/logomark.svg`, `/fonts/...`,
//!    anything with an extension) get the file directly with a long
//!    `Cache-Control: public, max-age=31536000, immutable` (Vite hashes
//!    the asset filenames so they're safe to cache forever).
//! 2. **Client-routed requests** (`/`, `/settings`, `/a/{kb}/{id}`, any
//!    URL the React Router knows about) get `dist/index.html` with
//!    `Cache-Control: no-cache` so the SPA shell is always re-validated.
//!
//! When the daemon was started without a SPA dist (no `web/dist` next to
//! the binary, no `KB_SPA_DIST` env var), we fall through with a friendly
//! 404 — this lets cargo-test runs of the daemon work without needing
//! the SPA to be built. The Playwright tests build it before booting.

use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderValue, Response, StatusCode, Uri},
    response::IntoResponse,
};
use kb_core::types::KbName;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// Parent-origin SPA fallback. Called only when the request didn't match
/// `/api/*` AND the Host header isn't an artifact subdomain (the
/// dispatcher in `dispatch.rs` does that branching).
pub async fn serve(State(state): State<Arc<KbHandles>>, uri: Uri) -> Response<Body> {
    let Some(spa_dist) = state.spa_dist.as_ref() else {
        return spa_unavailable();
    };
    let raw_path = uri.path();

    // Reject any path containing `..` segments outright. The SPA's
    // legitimate routes never use `..`, and `read_asset`'s canonicalize
    // check already prevents real escapes — this just stops us from
    // accidentally serving the shell for a request that obviously means
    // to traverse.
    if raw_path.split('/').any(|s| s == "..") {
        return not_found();
    }

    let trimmed = raw_path.trim_start_matches('/');
    let query = uri.query();
    let candidate = spa_dist.join(trimmed);
    if has_extension(trimmed) {
        match read_asset(spa_dist, &candidate) {
            Some(resp) => resp,
            // A `.html`-suffixed path that isn't a real asset can still
            // be a client route — a single-file artifact permalink
            // (`/a/{kb}/research/index.html`) or the multi-page sub-route
            // `/a/{kb}/{id}/{page}.html` (postMessage-driven, v0.6) both end
            // in `.html`. Serve the SPA shell (with OG meta) so React Router
            // can handle it; genuine missing assets elsewhere still 404.
            None if is_client_route(trimmed) => {
                serve_artifact_shell(spa_dist, &state, trimmed, query).await
            }
            None => not_found(),
        }
    } else if is_client_route(trimmed) {
        // Extensionless artifact permalink (`/a/{kb}/some-artifact`).
        serve_artifact_shell(spa_dist, &state, trimmed, query).await
    } else {
        serve_shell(spa_dist)
    }
}

/// Serve the SPA shell for an artifact permalink (`/a/{kb}/{source_rel}`) with
/// per-artifact OpenGraph + description `<meta>` injected into the static
/// `<head>` (OG1). The permalink is a byte-for-byte static shell, so client
/// `useDocumentTitle` is invisible to crawlers / unfurlers — we look the
/// artifact up at request time and splice escaped meta server-side. Best-effort
/// throughout: any miss (no dist, unknown kb, artifact not found) falls back to
/// the plain shell — meta is an enhancement, never a failure mode.
async fn serve_artifact_shell(
    spa_dist: &Path,
    state: &KbHandles,
    trimmed: &str,
    query: Option<&str>,
) -> Response<Body> {
    // F3b — when the live doc is gone but the moves table knows a new
    // source-rel, 301 to `/a/{kb}/{new_rel}` (preserve query string). Falls
    // through to the plain shell when moves also misses.
    if let Some(location) = moves_redirect_location(state, trimmed, query).await {
        return Response::builder()
            .status(StatusCode::MOVED_PERMANENTLY)
            .header(header::LOCATION, location.as_str())
            .body(Body::empty())
            .unwrap_or_else(|_| not_found());
    }

    // Read the shell from the mtime-guarded cache (sync; the lock is released
    // before the await below). The per-request OG splice (#34) stays: we
    // splice onto the cached bytes rather than re-reading from disk.
    let Some(bytes) = read_shell_bytes(spa_dist) else {
        return not_found();
    };
    let body = match artifact_meta_tags_for(state, trimmed).await {
        Some(tags) => inject_head_meta(&String::from_utf8_lossy(&bytes), &tags).into_bytes(),
        None => bytes.as_ref().clone(),
    };
    html_no_cache(body)
}

/// F3b — if `a/{kb}/{old_rel}` is not live but the moves log has a mapping,
/// return the absolute-path Location for a 301. Preserves the query string.
async fn moves_redirect_location(
    state: &KbHandles,
    trimmed: &str,
    query: Option<&str>,
) -> Option<String> {
    let rest = trimmed.strip_prefix("a/")?;
    let (kb_seg, source_rel) = rest.split_once('/')?;
    if source_rel.is_empty() {
        return None;
    }
    let kb_name = KbName::new(kb_seg).ok()?;
    let ctx = state.kbs.get(&kb_name)?;
    // Live hit → no redirect (caller serves shell + OG).
    let abs = kb_core::paths::canonical_abs(&ctx.source_path.join(source_rel));
    if ctx
        .storage
        .get_by_source_path(abs.to_string_lossy().into_owned())
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return None;
    }
    let (_new_id, new_rel) = kb_core::relocate::moves_lookup(&ctx.storage, source_rel)
        .await
        .ok()
        .flatten()?;
    if new_rel == source_rel {
        return None;
    }
    // Percent-encode each path segment (mirror web/src/lib/artifactHref.ts)
    // so spaces/% in new_rel produce a valid Location header. docs.rs
    // redirects use hex ids only — no encoding needed there.
    let enc_rel = encode_source_rel_for_location(&new_rel);
    let mut location = format!("/a/{kb_seg}/{enc_rel}");
    if let Some(q) = query {
        if !q.is_empty() {
            location.push('?');
            location.push_str(q);
        }
    }
    Some(location)
}

/// Encode each `/`-separated segment of a source-rel for a Location path
/// (RFC 3986 unreserved left bare — same set as `encodeURIComponent` for
/// typical path bytes: alnum + `-_.~`).
fn encode_source_rel_for_location(rel: &str) -> String {
    rel.split('/')
        .map(percent_encode_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn percent_encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let safe = b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~');
        if safe {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Look the artifact up from a `a/{kb}/{source_rel}` path and build its meta
/// tag block, or `None` if it can't be resolved. NOTE: the source-rel segment
/// is used verbatim (no percent-decode) — real artifact paths are URL-safe, and
/// a path with escaped bytes simply misses the lookup and gets no meta.
async fn artifact_meta_tags_for(state: &KbHandles, trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("a/")?;
    let (kb_seg, source_rel) = rest.split_once('/')?;
    if source_rel.is_empty() {
        return None;
    }
    let kb_name = KbName::new(kb_seg).ok()?;
    let ctx = state.kbs.get(&kb_name)?;
    // `get_by_source_path` matches the stored CANONICAL ABSOLUTE path (#27 — the
    // `path` column holds `canonical_abs`, despite the method name), so resolve
    // the URL's source-relative segment against the kb's source root first.
    let abs = kb_core::paths::canonical_abs(&ctx.source_path.join(source_rel));
    let doc = match ctx
        .storage
        .get_by_source_path(abs.to_string_lossy().into_owned())
        .await
        .ok()
        .flatten()
    {
        Some(d) => d,
        // Prefer OG for the redirected target when the old rel is only known
        // via the moves table (shell still 301s above for that case).
        None => {
            let (new_id, _) = kb_core::relocate::moves_lookup(&ctx.storage, source_rel)
                .await
                .ok()
                .flatten()?;
            ctx.storage.get_by_id(new_id).await.ok().flatten()?
        }
    };
    // Prefer the authored one-line `kb-summary`; else the body excerpt.
    let description = doc.kb_summary.as_deref().or(doc.summary.as_deref());
    Some(artifact_meta_tags(&doc.title, description))
}

/// Build the `<meta>` block for an artifact. All values are HTML-attribute
/// escaped (XSS-safe — title/summary come from user-edited artifact metadata).
fn artifact_meta_tags(title: &str, description: Option<&str>) -> String {
    let t = escape_html_attr(title);
    let mut s = String::new();
    s.push_str(&format!("<meta property=\"og:title\" content=\"{t}\">"));
    s.push_str("<meta property=\"og:type\" content=\"article\">");
    if let Some(d) = description {
        let d = escape_html_attr(d);
        s.push_str(&format!("<meta name=\"description\" content=\"{d}\">"));
        s.push_str(&format!(
            "<meta property=\"og:description\" content=\"{d}\">"
        ));
    }
    s
}

/// Splice `tags` immediately before `</head>`. No-op (returns the input) if the
/// shell has no `</head>` — we never corrupt the document. Only the parent
/// `<head>` is touched; the kb-prompt `<template>` convention (#5) is never in
/// the SPA shell, so there's nothing to scrub here.
fn inject_head_meta(html: &str, tags: &str) -> String {
    match html.find("</head>") {
        Some(idx) => {
            let mut out = String::with_capacity(html.len() + tags.len());
            out.push_str(&html[..idx]);
            out.push_str(tags);
            out.push_str(&html[idx..]);
            out
        }
        None => html.to_string(),
    }
}

/// HTML-attribute escape for splice-safety. Mirrors the minimal escape set used
/// across kb-core (`meta_edit`/`iframe`): `& < > "`.
fn escape_html_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn html_no_cache(body: Vec<u8>) -> Response<Body> {
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

/// True for SPA client routes that can legitimately carry a file
/// extension. Only `/a/{kb}/{id}/{splat}` does — its splat is a
/// sub-page filename like `01-timeline.html`.
fn is_client_route(trimmed: &str) -> bool {
    trimmed.starts_with("a/")
}

fn read_asset(root: &Path, candidate: &Path) -> Option<Response<Body>> {
    let resolved = candidate.canonicalize().ok()?;
    let root_canonical = root.canonicalize().ok()?;
    if !resolved.starts_with(&root_canonical) {
        return None;
    }
    let bytes = std::fs::read(&resolved).ok()?;
    let ct = super::guess_content_type(&resolved);
    let mut resp = (StatusCode::OK, bytes).into_response();
    resp.headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    // Vite emits content-hashed filenames for everything in /assets,
    // so they're safe to cache forever. Other assets (logomark.svg,
    // fonts, etc.) get a shorter window.
    let cache = if resolved
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("webmanifest"))
    {
        // The PWA manifest is the unhashed install descriptor — keep it
        // revalidated (like the shell) so manifest edits propagate promptly
        // instead of sitting in a 1h browser/installed-app cache.
        "no-cache"
    } else if resolved
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
    match read_shell_bytes(spa_dist) {
        Some(bytes) => html_no_cache(bytes.as_ref().clone()),
        None => not_found(),
    }
}

/// mtime-guarded cache of the SPA `index.html` bytes. The shell was re-read
/// from disk (blocking `std::fs::read` on the tokio worker) on every
/// parent-origin page load and every artifact permalink; caching drops that
/// to a single `metadata()` stat per request. Live `web/dist/index.html`
/// swaps (bind-mount redeploy without a daemon restart) stay visible: when
/// the file's mtime changes the cache is refreshed. Single-slot, keyed on
/// `(path, mtime)` — a daemon has one `spa_dist`, and multiple in-process
/// daemons share the same path/file so the slot never serves stale bytes.
struct ShellCache {
    path: PathBuf,
    mtime: SystemTime,
    bytes: Arc<Vec<u8>>,
}

static SHELL_CACHE: OnceLock<Mutex<Option<ShellCache>>> = OnceLock::new();

/// Return the SPA shell bytes, from cache when the file's mtime is unchanged.
/// `None` if `index.html` can't be read. The `std::sync::Mutex` is never held
/// across an `.await` (this fn is sync and returns before any await).
fn read_shell_bytes(spa_dist: &Path) -> Option<Arc<Vec<u8>>> {
    let shell = spa_dist.join("index.html");
    // A missing/unreadable mtime forces a fresh read (never serve stale when
    // freshness can't be verified) — degrades to the pre-cache behavior.
    let mtime = std::fs::metadata(&shell)
        .ok()
        .and_then(|m| m.modified().ok());
    let cell = SHELL_CACHE.get_or_init(|| Mutex::new(None));
    if let Some(mtime) = mtime {
        let guard = cell.lock().unwrap();
        if let Some(c) = guard.as_ref() {
            if c.path == shell && c.mtime == mtime {
                return Some(c.bytes.clone());
            }
        }
    }
    let bytes = Arc::new(std::fs::read(&shell).ok()?);
    if let Some(mtime) = mtime {
        let mut guard = cell.lock().unwrap();
        *guard = Some(ShellCache {
            path: shell,
            mtime,
            bytes: bytes.clone(),
        });
    }
    Some(bytes)
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
        "type": "urn:kb:errors:spa-unavailable",
        "title": "SPA not built",
        "status": 404,
        "detail": "The daemon was started without a SPA dist. Build with `cd web && npm run build` and restart, or set KB_SPA_DIST.",
    });
    let mut resp = (StatusCode::NOT_FOUND, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

/// Resolve the SPA dist directory at startup. Order:
///   1. `KB_SPA_DIST` env var (absolute path)
///   2. `web/dist` relative to the current working directory
///   3. None — daemon serves API only; SPA paths return 404.
pub fn resolve_spa_dist() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("KB_SPA_DIST") {
        let pb = PathBuf::from(p);
        if pb.join("index.html").is_file() {
            return Some(pb);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join("web").join("dist");
    if candidate.join("index.html").is_file() {
        Some(candidate)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHELL: &str =
        "<html><head><title>kb</title></head><body><div id=\"root\"></div></body></html>";

    // invariant:34 og-meta-splice
    #[test]
    fn injects_og_meta_before_head_close() {
        let tags = artifact_meta_tags("My Doc", Some("A one-line gloss"));
        let out = inject_head_meta(SHELL, &tags);
        assert!(out.contains("<meta property=\"og:title\" content=\"My Doc\">"));
        assert!(out.contains("<meta property=\"og:type\" content=\"article\">"));
        assert!(out.contains("<meta name=\"description\" content=\"A one-line gloss\">"));
        assert!(out.contains("<meta property=\"og:description\" content=\"A one-line gloss\">"));
        // Spliced INSIDE the head (before </head>), body untouched.
        assert!(out.find("og:title").unwrap() < out.find("</head>").unwrap());
        assert!(out.contains("<body><div id=\"root\"></div></body>"));
    }

    #[test]
    fn shell_cache_serves_cached_bytes_and_refreshes_on_mtime_change() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("index.html");
        std::fs::write(&shell, b"<html><head></head><body>v1</body></html>").unwrap();

        let first = read_shell_bytes(dir.path()).expect("read v1");
        assert!(first.windows(2).any(|w| w == b"v1"));
        // Second read with an unchanged file returns the same cached Arc.
        let second = read_shell_bytes(dir.path()).expect("read cached");
        assert!(Arc::ptr_eq(&first, &second));

        // Rewrite with a strictly newer mtime → cache must refresh.
        let newer = SystemTime::now() + std::time::Duration::from_secs(5);
        {
            let mut f = std::fs::File::create(&shell).unwrap();
            f.write_all(b"<html><head></head><body>v2</body></html>")
                .unwrap();
        }
        std::fs::File::open(&shell)
            .unwrap()
            .set_modified(newer)
            .unwrap();
        let third = read_shell_bytes(dir.path()).expect("read v2");
        assert!(third.windows(2).any(|w| w == b"v2"));
        assert!(!Arc::ptr_eq(&second, &third));
    }

    // invariant:34 xss-escape
    #[test]
    fn escapes_xss_in_meta_values() {
        let tags = artifact_meta_tags(
            "<script>alert(1)</script>",
            Some("quote \" amp & lt < gt >"),
        );
        // No raw angle brackets / quotes from the artifact values survive into
        // the attribute — only our own tag syntax.
        assert!(!tags.contains("<script>"));
        assert!(tags.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(tags.contains("&quot;"));
        assert!(tags.contains("&amp;"));
        assert!(tags.contains("lt &lt; gt &gt;"));
    }

    #[test]
    fn omits_description_when_none() {
        let tags = artifact_meta_tags("Title only", None);
        assert!(tags.contains("og:title"));
        assert!(tags.contains("og:type"));
        assert!(!tags.contains("description"));
    }

    #[test]
    fn inject_is_noop_without_head() {
        let html = "<html><body>no head here</body></html>";
        assert_eq!(inject_head_meta(html, "<meta x>"), html);
    }

    #[test]
    fn escape_html_attr_covers_the_set() {
        assert_eq!(escape_html_attr("a&b<c>d\"e"), "a&amp;b&lt;c&gt;d&quot;e");
        assert_eq!(escape_html_attr("plain text"), "plain text");
    }

    /// T8 — path segments with spaces/% are percent-encoded for Location.
    #[test]
    fn encode_source_rel_encodes_space_per_segment() {
        assert_eq!(
            encode_source_rel_for_location("has space.html"),
            "has%20space.html"
        );
        assert_eq!(
            encode_source_rel_for_location("dir/has space.html"),
            "dir/has%20space.html"
        );
        assert_eq!(
            encode_source_rel_for_location("safe-file.html"),
            "safe-file.html"
        );
        // Slash stays a separator; only segments are encoded.
        assert_eq!(
            encode_source_rel_for_location("a b/c d.html"),
            "a%20b/c%20d.html"
        );
    }
}
