//! Iframe sandbox primitives — lifted from `spike-iframe` (which validated
//! the cluster-1 resolution end-to-end with Playwright: 16/16 probes pass
//! in Chromium across 4 canon artifacts). Production differences:
//!
//! - Probe injection uses `lol_html::HtmlRewriter` for streaming, large-
//!   file-safe rewriting (the spike used text-replacement, fine for
//!   throwaway code but not for production large artifacts). Per
//!   spike-iframe §"What I would NOT carry forward".
//! - `parse_artifact_id` is lifted **verbatim** including the path-
//!   traversal rejection unit tests, plus a new proptest that asserts
//!   the property over arbitrary host strings.
//!
//! Topic 06 §Decisions: subdomain per artifact (`<id>.artifacts.localhost`)
//! gives each iframe a distinct Origin; the sandbox attribute keeps
//! `allow-same-origin` (required for `localStorage`) — isolation comes
//! from the subdomain, not from removing the flag.

use crate::Result;
use lol_html::html_content::ContentType;
use lol_html::{element, HtmlRewriter, Settings};

/// Sandbox flag string from topic 06 §Decisions cluster-1 resolution.
///
/// `allow-same-origin` IS in the list — without it `localStorage` throws
/// `SecurityError`. Origin isolation comes from the subdomain giving each
/// artifact its own real origin (CodePen / Observable / Claude-Artifacts
/// pattern), not from the sandbox attribute alone.
pub const SANDBOX_FLAGS: &str = "allow-scripts allow-same-origin allow-popups \
                                 allow-popups-to-escape-sandbox allow-forms \
                                 allow-modals allow-downloads";

/// Default suffix used when the caller doesn't pass one (tests, proptest).
/// Production uses the `[server] artifact_host_suffix` config field.
pub const DEFAULT_HOST_SUFFIX: &str = ".artifacts.localhost";

/// Parse the artifact-host LABEL (the part of `Host:` before `suffix`) out
/// of an HTTP `Host:` header value.
///
/// Returns `Some(label)` if the host has shape `<label><suffix>[:port]` with
/// `label` matching `[a-z0-9._-]+` and not starting/ending with `.` and not
/// containing `..`. Returns `None` for the parent page host or anything
/// else. `suffix` is the configured artifact-subdomain suffix (e.g.
/// `.artifacts.localhost` for dev, `.artifacts.example.com` for production).
///
/// This is the ARTIFACT HOST GRAMMAR v1 (bare) extraction — kept exactly as
/// it always behaved (character class, `..`/leading-dot rejection) so
/// existing callers (`kb_core::parser::extract_artifact_link_target`,
/// `kb_core::share::classify_cross_link`) stay byte-identical. `label` may
/// itself further parse as a v2 kb-qualified label
/// (`{kb_enc}--{id}` — [`parse_artifact_host_id`]); this function doesn't
/// care either way, it just extracts the raw text between the suffix and
/// the host's start.
pub fn parse_artifact_id<'h>(host: &'h str, suffix: &str) -> Option<&'h str> {
    // Strip the port, if present.
    let host_no_port = host.split(':').next().unwrap_or(host);
    let id = host_no_port.strip_suffix(suffix)?;
    if id.is_empty() {
        return None;
    }
    // Reject ids starting or ending with a dot — those produce path traversals.
    if id.starts_with('.') || id.ends_with('.') {
        return None;
    }
    if id.contains("..") {
        return None;
    }
    // Only accept ids that look "safe" for a filesystem mapping —
    // alphanumerics, dashes, underscores, dots. No slashes.
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return None;
    }
    Some(id)
}

/// A parsed artifact-host LABEL — ARTIFACT HOST GRAMMAR v2. Server AND
/// client parse this identically (both golden-pinned); see
/// `docs/architecture-invariants.md` #2 / #7 neighbourhood and the CT
/// milestone plan for the full grammar writeup.
///
/// - **`Bare`** — today's pre-v2 shape: a 12-hex artifact id or a legacy
///   file-stem. Byte-identical to what `parse_artifact_id` always returned.
/// - **`Qualified`** — `{kb_enc}--{id}`: `kb_enc` is a kb name with every
///   `_` replaced by `-` ([`encode_kb_name`]), `id` is exactly 12 lowercase
///   hex chars. Disambiguates artifacts that collide on id (two kbs
///   sharing a source-relative path hash to the same
///   `ArtifactId::from_path`) — `Bare` alone can't express "which kb".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactHostId {
    Bare(String),
    Qualified { kb_enc: String, id: String },
}

/// Encode a kb name for the qualified-label grammar: every `_` becomes
/// `-`. Kb names are `[a-z0-9_-]+` (`KbName::new`); the artifact-host label
/// is a single DNS label, so the (legal-in-a-kb-name, non-standard-in-a-
/// hostname) `_` is folded to `-`. This is LOSSY on purpose — `foo_bar` and
/// `foo-bar` encode to the same `kb_enc` — which is why server-side
/// resolution is a linear scan over configured kbs (comparing each kb's
/// OWN encoded name) rather than a direct lookup.
pub fn encode_kb_name(kb_name: &str) -> String {
    kb_name.replace('_', "-")
}

/// Build a v2 qualified artifact-host LABEL (`{kb_enc}--{id}`, no suffix)
/// from a raw kb name + artifact id. The one place that knows the
/// grammar's separator and encoding — server-side label construction and
/// the golden tests below share this instead of ad-hoc `format!`s.
pub fn qualified_label(kb_name: &str, id: &str) -> String {
    format!("{}--{}", encode_kb_name(kb_name), id)
}

/// Exactly 12 lowercase hex chars (`kb_core::ids::ArtifactId`'s shape).
fn is_hex12_lower(s: &str) -> bool {
    s.len() == 12
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `[a-z0-9][a-z0-9-]*` — a non-empty, DNS-label-safe `kb_enc`.
fn is_valid_kb_enc(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parse the ARTIFACT HOST GRAMMAR v2. `suffix` is the same configured
/// `artifact_host_suffix` [`parse_artifact_id`] takes.
///
/// First extracts the label via `parse_artifact_id` (so every v1
/// rejection — wrong suffix, path traversal shapes, empty label — still
/// applies identically). Then, if the label ends with `--` + exactly 12
/// lowercase hex chars AND the prefix before that `--` is a non-empty
/// valid `kb_enc`, returns `Qualified`. The split is at the RIGHTMOST
/// `--` in the label — so a `kb_enc` that itself contains `--` (a kb name
/// with an adjacent `_`/`-` pair, e.g. `foo__bar` encodes to `foo--bar`)
/// still parses deterministically: the id always occupies the LAST
/// 12-hex segment. Anything that doesn't fit — no `--`, a wrong-length or
/// uppercase trailing segment, an empty or invalid `kb_enc` — falls back
/// to `Bare(label)`, byte-identical to pre-v2 behaviour. This function
/// does NOT check whether any configured kb actually encodes to `kb_enc`
/// — that's the server's resolution step, not the grammar.
pub fn parse_artifact_host_id(host: &str, suffix: &str) -> Option<ArtifactHostId> {
    let label = parse_artifact_id(host, suffix)?;
    if let Some(idx) = label.rfind("--") {
        let kb_enc = &label[..idx];
        let id = &label[idx + 2..];
        if !kb_enc.is_empty() && is_valid_kb_enc(kb_enc) && is_hex12_lower(id) {
            return Some(ArtifactHostId::Qualified {
                kb_enc: kb_enc.to_string(),
                id: id.to_string(),
            });
        }
    }
    Some(ArtifactHostId::Bare(label.to_string()))
}

/// Inject a `<script src="...">` tag just before `</body>` using lol-html's
/// streaming rewriter. Falls back to appending the script + a synthetic
/// `</body>` at the end of the input if the document has no `<body>`.
pub fn inject_script(html: &str, script_src: &str) -> Result<String> {
    let injection = format!(
        r#"<script src="{}" defer></script>"#,
        escape_attr(script_src)
    );
    let mut output: Vec<u8> = Vec::with_capacity(html.len() + injection.len());
    let mut body_seen = false;
    {
        let body_seen_ref = &mut body_seen;
        let injection_ref = &injection;
        let mut rewriter = HtmlRewriter::new(
            Settings::new().append_element_content_handler(element!("body", move |el| {
                *body_seen_ref = true;
                el.append(injection_ref, ContentType::Html);
                Ok(())
            })),
            |c: &[u8]| output.extend_from_slice(c),
        );
        rewriter
            .write(html.as_bytes())
            .map_err(|e| crate::Error::BadRequest(format!("html rewrite (write): {e}")))?;
        rewriter
            .end()
            .map_err(|e| crate::Error::BadRequest(format!("html rewrite (end): {e}")))?;
    }

    let mut s = String::from_utf8(output)
        .map_err(|e| crate::Error::Internal(anyhow::anyhow!("rewriter output: {e}")))?;
    if !body_seen {
        // No <body> in the document — append at the end so consumers still
        // get the probe loaded.
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&injection);
        s.push('\n');
    }
    Ok(s)
}

/// Inject the v0.2 annotator: an inline data block carrying the current
/// kb-comments/1 payload, followed by the deferred annotator script.
///
/// Both inserts land just before `</body>` via lol-html. Defer ordering
/// (spec: deferred scripts run in document order, after parsing) ensures
/// the inline data block has executed and `window.__KB_COMMENTS` is set
/// by the time `annotate.js` runs.
///
/// `payload` should be a serializable kb-comments/1 envelope; it'll be
/// JSON-encoded inline. `script_src` is the URL the annotator is fetched
/// from — usually `/_kb/annotate.js` on the artifact subdomain.
///
/// Falls back to appending at end-of-document when no `<body>` is present
/// (some artifacts skip the explicit body tag).
pub fn inject_annotator(
    html: &str,
    payload: &serde_json::Value,
    script_src: &str,
    parent_origin: &str,
) -> Result<String> {
    // serde_json doesn't escape `<` or `/`, so a user-authored comment body
    // containing `</script>` would terminate this script element early and
    // let injected HTML execute at the artifact-subdomain origin. Replacing
    // `</` with `<\/` is a no-op inside a JS string (the lexer drops the
    // backslash) but defeats the HTML close-tag match.
    let json = serde_json::to_string(payload)?.replace("</", "<\\/");
    // X2 — tell the annotator which origin its parent (the SPA) is, so it
    // can reject `cm:*` postMessages from any other embedder. `'*'` (the
    // dev default) means "accept any origin". JSON-encoded so it's a safe
    // JS string literal (and `</` escaped for the same reason as above).
    let parent_origin_js = serde_json::to_string(parent_origin)?.replace("</", "<\\/");
    let injection = format!(
        r#"<script>window.__KB_COMMENTS = {json};window.__KB_PARENT_ORIGIN = {parent_origin_js};</script><script src="{}" defer></script>"#,
        escape_attr(script_src)
    );
    let mut output: Vec<u8> = Vec::with_capacity(html.len() + injection.len());
    let mut body_seen = false;
    {
        let body_seen_ref = &mut body_seen;
        let injection_ref = &injection;
        let mut rewriter = HtmlRewriter::new(
            Settings::new().append_element_content_handler(element!("body", move |el| {
                *body_seen_ref = true;
                el.append(injection_ref, ContentType::Html);
                Ok(())
            })),
            |c: &[u8]| output.extend_from_slice(c),
        );
        rewriter
            .write(html.as_bytes())
            .map_err(|e| crate::Error::BadRequest(format!("html rewrite (annotator): {e}")))?;
        rewriter
            .end()
            .map_err(|e| crate::Error::BadRequest(format!("html rewrite (annotator end): {e}")))?;
    }
    let mut s = String::from_utf8(output)
        .map_err(|e| crate::Error::Internal(anyhow::anyhow!("rewriter output: {e}")))?;
    if !body_seen {
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&injection);
        s.push('\n');
    }
    Ok(s)
}

/// Inject an inline `<script>` as the **first** child of `<head>` so it
/// runs before the document body is parsed. Used for the top-level bounce
/// (a synchronous `location.replace` must fire before the raw artifact
/// paints when a tab is opened outside the kb SPA iframe).
///
/// `js` is daemon-generated, but we still apply the `inject_annotator`
/// `</` → `<\/` escape so a templated value containing a literal close-tag
/// can never terminate the script element early.
///
/// Falls back to prepending at the very start of the document when there
/// is no `<head>` (some artifacts skip it) — the script still runs first.
pub fn inject_inline_head_script(html: &str, js: &str) -> Result<String> {
    let injection = format!("<script>{}</script>", js.replace("</", "<\\/"));
    let mut output: Vec<u8> = Vec::with_capacity(html.len() + injection.len());
    let mut head_seen = false;
    {
        let head_seen_ref = &mut head_seen;
        let injection_ref = &injection;
        let mut rewriter = HtmlRewriter::new(
            Settings::new().append_element_content_handler(element!("head", move |el| {
                *head_seen_ref = true;
                el.prepend(injection_ref, ContentType::Html);
                Ok(())
            })),
            |c: &[u8]| output.extend_from_slice(c),
        );
        rewriter
            .write(html.as_bytes())
            .map_err(|e| crate::Error::BadRequest(format!("html rewrite (head write): {e}")))?;
        rewriter
            .end()
            .map_err(|e| crate::Error::BadRequest(format!("html rewrite (head end): {e}")))?;
    }
    let s = String::from_utf8(output)
        .map_err(|e| crate::Error::Internal(anyhow::anyhow!("rewriter output: {e}")))?;
    if !head_seen {
        // No <head> — prepend at document start so the bounce still runs first.
        return Ok(format!("{injection}\n{s}"));
    }
    Ok(s)
}

fn escape_attr(s: &str) -> String {
    // `&` first — escaping it after `"`/`<` would double-escape the `&`
    // inside the `&quot;` / `&lt;` entities those produce.
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- spike-iframe baseline tests (preserved) ----------------------------

    const SFX: &str = DEFAULT_HOST_SUFFIX;

    #[test]
    fn parse_artifact_id_extracts_subdomain() {
        assert_eq!(
            parse_artifact_id("s01.artifacts.localhost:4000", SFX),
            Some("s01")
        );
        assert_eq!(
            parse_artifact_id("kitchen-sink.artifacts.localhost:4000", SFX),
            Some("kitchen-sink")
        );
        assert_eq!(
            parse_artifact_id("fullscreen-viz.artifacts.localhost", SFX),
            Some("fullscreen-viz")
        );
        assert_eq!(
            parse_artifact_id("multi.page.artifacts.localhost", SFX),
            Some("multi.page")
        );
    }

    // invariant:7 host-suffix
    #[test]
    fn parse_artifact_id_works_with_production_suffix() {
        // Operators set `[server] artifact_host_suffix = ".artifacts.example.com"`
        // when fronting the daemon with Caddy/Traefik on a real domain.
        let sfx = ".artifacts.example.com";
        assert_eq!(
            parse_artifact_id("kitchen-sink.artifacts.example.com", sfx),
            Some("kitchen-sink")
        );
        assert_eq!(
            parse_artifact_id("foo.artifacts.example.com:443", sfx),
            Some("foo")
        );
        // Wrong suffix → None (no cross-suffix leakage).
        assert_eq!(parse_artifact_id("foo.artifacts.localhost", sfx), None);
    }

    #[test]
    fn parse_artifact_id_rejects_non_subdomain_hosts() {
        assert_eq!(parse_artifact_id("localhost:4000", SFX), None);
        assert_eq!(parse_artifact_id("127.0.0.1:4000", SFX), None);
        assert_eq!(parse_artifact_id("example.com", SFX), None);
        assert_eq!(parse_artifact_id(".artifacts.localhost", SFX), None);
        assert_eq!(parse_artifact_id("artifacts.localhost", SFX), None);
    }

    #[test]
    fn parse_artifact_id_rejects_path_traversal() {
        assert_eq!(parse_artifact_id("..artifacts.localhost", SFX), None);
        assert_eq!(
            parse_artifact_id("..%2Fevil.artifacts.localhost", SFX),
            None
        );
        assert_eq!(parse_artifact_id("foo/bar.artifacts.localhost", SFX), None);
        assert_eq!(parse_artifact_id("..foo.artifacts.localhost", SFX), None);
        assert_eq!(
            parse_artifact_id("foo...bar.artifacts.localhost", SFX),
            None
        );
    }

    #[test]
    fn sandbox_flags_include_allow_same_origin() {
        assert!(SANDBOX_FLAGS.contains("allow-scripts"));
        assert!(SANDBOX_FLAGS.contains("allow-same-origin"));
        // Cluster-1 fix: allow-same-origin IS present; storage works because
        // the subdomain gives each artifact a distinct Origin.
    }

    // --- lol-html injection tests ------------------------------------------

    #[test]
    fn inject_script_inserts_before_body_close() {
        let html = "<html><body><p>hello</p></body></html>";
        let out = inject_script(html, "/_kb/probe.js").unwrap();
        assert!(out.contains(r#"<script src="/_kb/probe.js" defer></script></body>"#));
        assert!(out.contains("<p>hello</p>"));
    }

    #[test]
    fn inject_script_appends_when_no_body() {
        let html = "<p>orphan</p>";
        let out = inject_script(html, "/_kb/probe.js").unwrap();
        assert!(out.contains("<p>orphan</p>"));
        assert!(out.contains(r#"<script src="/_kb/probe.js" defer></script>"#));
    }

    #[test]
    fn inject_script_handles_complex_documents() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Test</title><style>body { color: red; }</style></head>
              <body>
                <h1>Header</h1>
                <p>Paragraph 1</p>
                <details><summary>Click</summary><p>Hidden</p></details>
                <script>console.log('inline');</script>
              </body>
            </html>
        "#;
        let out = inject_script(html, "/_kb/probe.js").unwrap();
        assert!(out.contains(r#"<script src="/_kb/probe.js" defer></script>"#));
        // Existing inline script preserved.
        assert!(out.contains("console.log('inline')"));
        // Title and structure preserved.
        assert!(out.contains("<title>Test</title>"));
    }

    #[test]
    fn inject_script_escapes_attribute() {
        let html = "<html><body></body></html>";
        let out = inject_script(html, "evil\" onerror=\"alert(1)\"").unwrap();
        assert!(!out.contains("onerror=\"alert(1)\""));
        assert!(out.contains("&quot;"));
    }

    #[test]
    fn inject_annotator_inserts_data_block_then_deferred_script() {
        let html = "<html><body><p>hi</p></body></html>";
        let payload = serde_json::json!({
            "schema": "kb-comments/1",
            "comments": [{"id": "c-1", "body": "first comment"}]
        });
        let out =
            inject_annotator(html, &payload, "/_kb/annotate.js", "https://kb.example").unwrap();
        assert!(out.contains(r#"<script>window.__KB_COMMENTS = "#));
        assert!(out.contains(r#""body":"first comment""#));
        // X2 — the parent origin is injected so the annotator can gate
        // incoming cm:* messages on it.
        assert!(out.contains(r#"window.__KB_PARENT_ORIGIN = "https://kb.example""#));
        assert!(out.contains(r#"<script src="/_kb/annotate.js" defer></script>"#));
        assert!(out.contains("<p>hi</p>"));
        // Data block must come before the deferred script so the defer
        // ordering guarantee leaves __KB_COMMENTS populated by run time.
        let data_idx = out.find("window.__KB_COMMENTS").unwrap();
        let script_idx = out.find(r#"src="/_kb/annotate.js""#).unwrap();
        assert!(data_idx < script_idx, "data block must precede script tag");
    }

    #[test]
    fn inject_annotator_falls_back_when_no_body() {
        let html = "<p>orphan</p>";
        let payload = serde_json::json!({"schema": "kb-comments/1", "comments": []});
        let out =
            inject_annotator(html, &payload, "/_kb/annotate.js", "https://kb.example").unwrap();
        assert!(out.contains("<p>orphan</p>"));
        assert!(out.contains("window.__KB_COMMENTS"));
        assert!(out.contains(r#"<script src="/_kb/annotate.js" defer></script>"#));
    }

    #[test]
    fn inject_annotator_escapes_close_script_in_json_payload() {
        // A user-authored comment body containing literally `</script>` must
        // not be able to break out of the inline data block. Without the
        // `</` → `<\/` escape, this payload terminates the first <script>
        // and the rest of the body becomes executable HTML at the artifact-
        // subdomain origin (stored XSS).
        let html = "<html><body></body></html>";
        let payload = serde_json::json!({
            "schema": "kb-comments/1",
            "comments": [{
                "id": "c-1",
                "body": "</script><script>window.__pwn = 1;</script>"
            }]
        });
        let out =
            inject_annotator(html, &payload, "/_kb/annotate.js", "https://kb.example").unwrap();
        // The literal close-script substring must not appear inside the data block.
        // (The only legitimate `</script>` is the one closing our inline script.)
        assert!(
            !out.contains(r#"</script><script>window.__pwn"#),
            "comment body broke out of data block: {out}"
        );
        // The escaped form `<\/script>` IS present (no-op for the JS lexer,
        // unparseable by the HTML close-tag scanner).
        assert!(
            out.contains(r"<\/script>"),
            "expected escaped </script> in JSON, got: {out}"
        );
        // Exactly two `</script>` close tags: one for the data block, one
        // for the deferred annotator src tag. No third one from the payload.
        let close_count = out.matches("</script>").count();
        assert_eq!(
            close_count, 2,
            "expected 2 </script> tags, got {close_count}: {out}"
        );
    }

    #[test]
    fn inject_annotator_escapes_script_src_attribute() {
        let html = "<html><body></body></html>";
        let payload = serde_json::json!({"schema": "kb-comments/1", "comments": []});
        let out = inject_annotator(
            html,
            &payload,
            "evil\" onerror=\"alert(1)\"",
            "https://kb.example",
        )
        .unwrap();
        assert!(!out.contains("onerror=\"alert(1)\""));
        assert!(out.contains("&quot;"));
    }

    #[test]
    fn inject_inline_head_script_prepends_into_head() {
        let html = "<html><head><title>T</title></head><body><p>hi</p></body></html>";
        let out = inject_inline_head_script(html, "location.replace('/x');").unwrap();
        assert!(out.contains("<script>location.replace('/x');</script>"));
        // Must run before the existing <title> (prepend, not append).
        let script_idx = out.find("location.replace").unwrap();
        let title_idx = out.find("<title>").unwrap();
        assert!(
            script_idx < title_idx,
            "bounce must precede head contents: {out}"
        );
        assert!(out.contains("<p>hi</p>"));
    }

    #[test]
    fn inject_inline_head_script_escapes_close_tag() {
        let html = "<html><head></head><body></body></html>";
        // A templated value with a literal close-tag must not break out.
        let out = inject_inline_head_script(html, "var x = \"a</script>b\";").unwrap();
        assert!(!out.contains("a</script>b"), "close-tag broke out: {out}");
        assert!(out.contains(r"a<\/script>b"));
        // Exactly one real </script> (the one closing our injected block).
        assert_eq!(out.matches("</script>").count(), 1, "{out}");
    }

    #[test]
    fn inject_inline_head_script_falls_back_when_no_head() {
        let html = "<p>orphan</p>";
        let out = inject_inline_head_script(html, "noop();").unwrap();
        assert!(out.contains("<script>noop();</script>"));
        assert!(out.contains("<p>orphan</p>"));
        // Script comes first so it runs before any content.
        assert!(out.find("noop()").unwrap() < out.find("orphan").unwrap());
    }

    // --- ARTIFACT HOST GRAMMAR v2 (kb-qualified subdomains) ----------------

    #[test]
    fn host_id_bare_12_hex() {
        assert_eq!(
            parse_artifact_host_id("0eb547304658.artifacts.localhost", SFX),
            Some(ArtifactHostId::Bare("0eb547304658".to_string()))
        );
    }

    #[test]
    fn host_id_bare_legacy_stem() {
        assert_eq!(
            parse_artifact_host_id("kitchen-sink.artifacts.localhost:4000", SFX),
            Some(ArtifactHostId::Bare("kitchen-sink".to_string()))
        );
    }

    #[test]
    fn host_id_qualified_simple() {
        assert_eq!(
            parse_artifact_host_id("docs--0eb547304658.artifacts.localhost", SFX),
            Some(ArtifactHostId::Qualified {
                kb_enc: "docs".to_string(),
                id: "0eb547304658".to_string(),
            })
        );
        // Round-trips through the builder.
        assert_eq!(
            qualified_label("docs", "0eb547304658"),
            "docs--0eb547304658"
        );
    }

    #[test]
    fn host_id_qualified_kb_with_underscore_is_encoded() {
        // `_` -> `-`; both the builder and the parser agree.
        let label = qualified_label("obs_docs", "0eb547304658");
        assert_eq!(label, "obs-docs--0eb547304658");
        assert_eq!(
            parse_artifact_host_id(&format!("{label}{SFX}"), SFX),
            Some(ArtifactHostId::Qualified {
                kb_enc: "obs-docs".to_string(),
                id: "0eb547304658".to_string(),
            })
        );
    }

    #[test]
    fn host_id_qualified_kb_with_dash_is_unchanged() {
        // A kb name that already uses `-` (no `_`) encodes to itself — the
        // same `kb_enc` a `_`-named sibling could ALSO produce (encoding is
        // deliberately lossy; server resolution handles the ambiguity).
        let label = qualified_label("obs-docs", "0eb547304658");
        assert_eq!(label, "obs-docs--0eb547304658");
        assert_eq!(
            parse_artifact_host_id(&format!("{label}{SFX}"), SFX),
            Some(ArtifactHostId::Qualified {
                kb_enc: "obs-docs".to_string(),
                id: "0eb547304658".to_string(),
            })
        );
    }

    #[test]
    fn host_id_kb_enc_containing_double_dash_splits_at_rightmost() {
        // A kb name with an adjacent `_`/`-` pair (e.g. `foo__bar`) encodes
        // to a `kb_enc` that ITSELF contains `--`. The id must still be
        // recovered from the LAST 12-hex segment, not the first `--`.
        let label = qualified_label("foo__bar", "0eb547304658");
        assert_eq!(label, "foo--bar--0eb547304658");
        assert_eq!(
            parse_artifact_host_id(&format!("{label}{SFX}"), SFX),
            Some(ArtifactHostId::Qualified {
                kb_enc: "foo--bar".to_string(),
                id: "0eb547304658".to_string(),
            })
        );
    }

    #[test]
    fn host_id_looks_qualified_but_bad_grammar_falls_back_to_bare() {
        // Trailing segment isn't exactly 12 lowercase hex (too short) —
        // the WHOLE label is treated as Bare, not split.
        assert_eq!(
            parse_artifact_host_id("docs--abc123.artifacts.localhost", SFX),
            Some(ArtifactHostId::Bare("docs--abc123".to_string()))
        );
        // Uppercase hex is rejected too (grammar is lowercase-only).
        assert_eq!(
            parse_artifact_host_id("docs--0EB547304658.artifacts.localhost", SFX),
            Some(ArtifactHostId::Bare("docs--0EB547304658".to_string()))
        );
    }

    #[test]
    fn host_id_empty_kb_enc_rejected() {
        assert_eq!(
            parse_artifact_host_id("--0eb547304658.artifacts.localhost", SFX),
            Some(ArtifactHostId::Bare("--0eb547304658".to_string()))
        );
    }

    #[test]
    fn host_id_dotted_legacy_labels_still_bare() {
        assert_eq!(
            parse_artifact_host_id("multi.page.artifacts.localhost", SFX),
            Some(ArtifactHostId::Bare("multi.page".to_string()))
        );
    }

    #[test]
    fn host_id_none_when_not_an_artifact_subdomain() {
        // Every v1 rejection (wrong suffix, path traversal, bare parent
        // host) still applies identically through the v2 entry point.
        assert_eq!(parse_artifact_host_id("localhost:4000", SFX), None);
        assert_eq!(
            parse_artifact_host_id("foo/bar.artifacts.localhost", SFX),
            None
        );
    }

    #[test]
    fn encode_kb_name_replaces_every_underscore() {
        assert_eq!(encode_kb_name("obs_docs_v2"), "obs-docs-v2");
        assert_eq!(encode_kb_name("already-dashed"), "already-dashed");
    }

    // --- Property test (path traversal safety) -----------------------------
    //
    // Given any host string, if parse_artifact_id returns Some(id), then
    // joining id + ".html" to a tempdir must produce a path that stays
    // inside that tempdir. Any future relaxation of parse_artifact_id that
    // breaks this property is an HN-thread-class bug.
    proptest::proptest! {
        #[test]
        fn parse_artifact_id_never_escapes_canon_root(host in "[\\PC]{1,80}") {
            if let Some(id) = parse_artifact_id(&host, SFX) {
                let tmp = tempfile::tempdir().unwrap();
                let canon_root = tmp.path().to_path_buf();
                let candidate = canon_root.join(format!("{id}.html"));
                let canonicalized = candidate.parent().unwrap();
                // The parent of the candidate file must be the canon root —
                // no `..`, no slashes, no symlink shenanigans.
                assert_eq!(canonicalized, canon_root,
                    "id {id:?} resolves outside canon root: {candidate:?}");
                // The id must not contain a path separator.
                assert!(!id.contains('/'), "id {id:?} contains slash");
                assert!(!id.contains('\\'), "id {id:?} contains backslash");
            }
        }
    }
}
