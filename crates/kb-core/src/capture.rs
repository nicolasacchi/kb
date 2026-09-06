//! U1 (v0.25 quick capture) — the capture engine. Turns an uploaded file (or
//! a URL/text share) into a REAL file inside a kb's `capture/` folder,
//! stamped with provenance metadata at write time. Pure — no HTTP, no
//! multipart parsing, no auth; `kb-server`'s `routes/capture.rs` (U2) is the
//! only caller, and it owns extension-map gating (`ExtensionMap::pipeline`)
//! and body-size limits. "Physically real, semantically staged" (see the
//! milestone plan): a capture is an ordinary artifact everywhere downstream
//! (gallery, search, versions, share) — the watcher indexes it like any
//! other file once this module's `write_atomic` call lands.
//!
//! Two write paths:
//! - [`capture`] — an uploaded file, already pipeline-resolved by the
//!   caller (D1 — [`crate::extmap::Pipeline`] is exactly `Html` or
//!   `Markdown`, never a third parser).
//! - [`capture_url_stub`] — no file, just a URL and/or shared text: built
//!   into a small `.md` stub (`kind:url-stub` tag) via [`build_url_stub`].
//!   The daemon NEVER fetches the URL itself (SSRF ruling, milestone
//!   non-goals) — enrichment is agent-layer work, later.

use crate::extmap::Pipeline;
use crate::ids::ArtifactId;
use crate::{Error, Result};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Default per-kb capture subfolder (`KbSection::capture_dir` override).
pub const DEFAULT_CAPTURE_DIR: &str = "capture";

/// Fixed per-kb subfolder for `kb desk` handoff drafts. Unlike
/// [`DEFAULT_CAPTURE_DIR`], this is not operator-overridable in v1 — a
/// desk corpus is an ordinary `[kb.*]` whose `handoff/` directory the
/// engine writes into.
pub const DESK_DIR: &str = "handoff";

/// Default `[server.capture].max_file_bytes` — same default as
/// `[server.attachments]` (`crate::attachments::DEFAULT_MAX_FILE_BYTES`), a
/// distinct knob for a distinct upload surface.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Default `[server.capture].max_request_bytes` — U2 follow-up: the
/// endpoint accepts a MULTI-FILE batch (see `routes::capture::MAX_CAPTURE_FILES`),
/// so the per-request body budget must be its own knob, distinct from
/// `max_file_bytes` (which bounds only ONE file). 64 MiB comfortably fits
/// several files under the default 10 MiB per-file cap while still bounding
/// a request.
pub const DEFAULT_MAX_REQUEST_BYTES: u64 = 64 * 1024 * 1024;

/// One uploaded-file capture, already pipeline-resolved by the caller (U2
/// gates on the kb's `ExtensionMap` — this module owns none of that
/// policy). Every `&str`/`&[u8]` field except `capture_dir` and `ext` is
/// caller-supplied over HTTP and therefore ATTACKER-CONTROLLED on a
/// non-loopback bind; see the traversal/XSS notes on [`capture_slug`] and
/// [`stamp_html_head`].
pub struct CaptureInput<'a> {
    /// Kb source root (already resolved by the caller).
    pub source_root: &'a Path,
    /// Per-kb capture subfolder, e.g. `KbSection::resolved_capture_dir()`.
    pub capture_dir: &'a str,
    /// The parse pipeline this content will be indexed under — selects the
    /// stamping strategy (front-matter vs head-splice) and whether
    /// `sanitize` applies (Html only; Markdown sanitize is a no-op v1 —
    /// milestone non-goals).
    pub pipeline: Pipeline,
    /// Extension for the OUTPUT filename, dot-less + lowercase (the
    /// caller's resolved extension — may differ from the pipeline's
    /// canonical extension when a kb maps a custom one onto a pipeline,
    /// e.g. `txt = "markdown"`). Falls back to the pipeline's canonical
    /// extension if empty/non-alphanumeric.
    pub ext: &'a str,
    /// Raw uploaded bytes, already read from the multipart stream/stdin —
    /// this module does no I/O beyond the final atomic write. Lossily
    /// decoded as UTF-8 (both pipelines are text formats).
    pub bytes: &'a [u8],
    /// Original multipart filename, if any — ATTACKER-CONTROLLED. Used
    /// only to derive a filename stem (via [`capture_slug`], which strips
    /// every non-alphanumeric byte including `/` and `.` — a traversal
    /// attempt has nothing left to traverse WITH) and as the
    /// `kb-capture-original` provenance value; never joined onto a path
    /// directly.
    pub original_filename: Option<&'a str>,
    /// Caller-supplied title. Wins over `original_filename` for the
    /// filename stem when non-empty. NOT injected into the uploaded
    /// content's own title/frontmatter/`<title>` — captures preserve the
    /// uploaded file's own authored title; this only steers the filename.
    pub title: Option<&'a str>,
    /// Free-text tags, slugified here via [`crate::parser::slugify_tag`]
    /// (the same transform the indexer applies at parse time).
    pub tags: &'a [String],
    /// Provenance `from:<value>` tag (`cli` / `spa` / `share` — U2's
    /// concern to resolve; slugified defensively here since a caller MAY
    /// derive it from an attacker-controlled header).
    pub from: &'a str,
    /// Opt-in HTML sanitize (ammonia). Ignored outside `Pipeline::Html`.
    pub sanitize: bool,
    /// Source URL for a saved page / share — stamped as `kb-capture-url`
    /// when present. Stored as inert metadata only; never dereferenced.
    pub url: Option<&'a str>,
    /// Desk stable-name upsert: when `Some`, the output filename is
    /// `<slug>.<ext>` with NO `-<unix_secs>` suffix and NO collision
    /// loop. An existing file at that path is overwritten atomically
    /// (`fsx::write_atomic`). Empty-after-slug is [`Error::BadRequest`],
    /// never the `"capture"` fallback [`capture_slug`] uses. `None`
    /// keeps the unique-filename path byte-identical to pre-desk
    /// behavior.
    pub stable_name: Option<&'a str>,
    /// When `Some`, stamp `kb-session` at write time (Markdown
    /// frontmatter / HTML `<meta>`). Never overwrites an already-present
    /// `kb-session` in the uploaded content (mirrors
    /// `session_marker::stamp_kb_session`).
    pub session_id: Option<&'a str>,
    /// Display-only `kb-expires-at` unix seconds. Absent → no stamp
    /// (byte-identical output). v1 has no sweeper.
    pub expires_at: Option<u64>,
    /// Override `kb-category` (default `"capture"`). Desk passes
    /// `"handoff"`. `None` is byte-identical to the historical stamp.
    pub category: Option<&'a str>,
}

/// One URL/text share (no uploaded file) — see [`capture_url_stub`].
pub struct UrlStubInput<'a> {
    pub source_root: &'a Path,
    pub capture_dir: &'a str,
    pub title: Option<&'a str>,
    pub url: Option<&'a str>,
    pub text: Option<&'a str>,
    pub tags: &'a [String],
    pub from: &'a str,
}

/// A successfully-written capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedArtifact {
    /// The artifact id the indexer will assign on next debounce — #27's
    /// `ArtifactId::from_path(doc_rel_path(..))` derivation, computed here
    /// pre-index so the caller can respond before the watcher runs.
    pub id: String,
    /// Source-root-relative path of the written file.
    pub source_relative: String,
    /// Best-effort resolved title (frontmatter/`<title>`/H1, mirroring the
    /// indexer's own `fields.title.or(fields.h1)` fallback chain), else the
    /// filename stem.
    pub title: String,
    /// `false` when [`CaptureInput::stable_name`] overwrote a file that
    /// already existed. Always `true` on the unique-filename path.
    pub created: bool,
}

/// Write one uploaded-file capture: filename policy → provenance stamp →
/// (opt-in sanitize, Html only) → atomic write → id derivation. See
/// [`CaptureInput`] for the attacker-controlled-input notes. `input.tags`
/// is slugified here via [`build_tag_list`] — free-text, unlike the raw
/// `kind:url-stub` provenance tag [`capture_url_stub`] adds (never itself
/// re-slugified, or its `:` would collapse to a dash).
pub fn capture(input: CaptureInput) -> Result<CapturedArtifact> {
    let tags = build_tag_list(input.from, input.tags);
    write_capture(&input, &tags)
}

/// Write one URL/text share as a `.md` stub (`kind:url-stub` tag) via
/// [`build_url_stub`]. Sanitize never applies (Markdown, v1 no-op). The
/// daemon never fetches `url` — it is stored as inert text. The
/// `kind:url-stub` tag is appended AFTER [`build_tag_list`] slugifies the
/// user tags — going through [`capture`]'s public `tags` field would
/// slugify its `:` away (`slugify_tag` collapses every non-alphanumeric
/// run, colon included, to a single dash).
pub fn capture_url_stub(input: UrlStubInput) -> Result<CapturedArtifact> {
    let title = input
        .title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("Untitled capture");
    let body = build_url_stub(title, input.url, input.text);

    let mut tags = build_tag_list(input.from, input.tags);
    tags.push("kind:url-stub".to_string());

    let cap = CaptureInput {
        source_root: input.source_root,
        capture_dir: input.capture_dir,
        pipeline: Pipeline::Markdown,
        ext: "md",
        bytes: body.as_bytes(),
        original_filename: None,
        title: Some(title),
        tags: input.tags,
        from: input.from,
        sanitize: false,
        url: input.url,
        stable_name: None,
        session_id: None,
        expires_at: None,
        category: None,
    };
    write_capture(&cap, &tags)
}

/// Shared write path: filename policy → (opt-in sanitize, Html only) →
/// provenance stamp → atomic write → id derivation. `tags` is the FINAL,
/// already-resolved tag list (no slugify here — both public entry points
/// resolve their own tags before calling this).
fn write_capture(input: &CaptureInput<'_>, tags: &[String]) -> Result<CapturedArtifact> {
    let ts = now_unix();
    let ext = effective_ext(input.pipeline, input.ext);
    let dir = input.source_root.join(input.capture_dir);
    let (filename, created) = if let Some(name) = input.stable_name {
        let slug = capture_slug_checked(name).ok_or_else(|| {
            Error::BadRequest(
                "stable name slugified to empty (need at least one alphanumeric)".into(),
            )
        })?;
        let filename = format!("{slug}.{ext}");
        let created = !dir.join(&filename).exists();
        (filename, created)
    } else {
        let base = base_stem(input.title, input.original_filename);
        (unique_filename(&dir, &base, &ext, ts), true)
    };
    let abs = dir.join(&filename);

    let decoded = String::from_utf8_lossy(input.bytes).into_owned();
    let sanitized = if input.sanitize && input.pipeline == Pipeline::Html {
        sanitize_html(&decoded)
    } else {
        decoded
    };

    let extra = StampExtra {
        session_id: input.session_id,
        expires_at: input.expires_at,
        category: input
            .category
            .filter(|s| !s.is_empty())
            .unwrap_or("capture"),
    };
    let stamped = match input.pipeline {
        Pipeline::Markdown => stamp_markdown_ex(
            &sanitized,
            tags,
            input.original_filename,
            input.url,
            ts,
            extra,
        ),
        Pipeline::Html => stamp_html_ex(
            &sanitized,
            tags,
            input.original_filename,
            input.url,
            ts,
            extra,
        ),
    };

    crate::fsx::write_atomic(&abs, stamped.as_bytes())?;

    // #27 — the SAME derivation `indexer::identity_for` uses: the artifact
    // id is the sha256-prefix-12 of the source-ROOT-RELATIVE path (both
    // sides canonicalised by `doc_rel_path`), never a content hash — so a
    // capture's id is stable across later edits, and matches what the
    // watcher will assign on its next debounce.
    let rel = crate::paths::doc_rel_path(&abs.to_string_lossy(), input.source_root);
    let id = ArtifactId::from_path(&rel).to_string();

    let title = resolved_title(input.pipeline, &stamped, &filename);

    Ok(CapturedArtifact {
        id,
        source_relative: rel,
        title,
        created,
    })
}

/// Build the Markdown BODY (no frontmatter — [`capture`] stamps that) for
/// a URL/text share: an H1 title, the URL as a Markdown autolink when
/// present, and any shared text as a paragraph. No escaping beyond what
/// Markdown authoring already implies — this is the artifact's own
/// content, not HTML being spliced into a trusted document (unlike
/// [`stamp_html_head`], which handles genuinely untrusted attribute
/// values).
pub fn build_url_stub(title: &str, url: Option<&str>, text: Option<&str>) -> String {
    let mut out = format!("# {title}\n");
    if let Some(u) = url.map(str::trim).filter(|u| !u.is_empty()) {
        out.push_str(&format!("\n<{u}>\n"));
    }
    if let Some(t) = text.map(str::trim).filter(|t| !t.is_empty()) {
        out.push_str(&format!("\n{t}\n"));
    }
    out
}

// --- filename policy ---------------------------------------------------------

/// Filename-safe slug for a capture's base name — same shape as
/// `memory::memory_slug` (lowercase ASCII-alnum runs joined by `-`, capped
/// at 60 chars) but with a capture-flavoured empty fallback. Every
/// non-alphanumeric byte — including `/`, `\`, and `.` — collapses to a
/// dash, so a path-traversal attempt embedded in an uploaded filename
/// (`../../etc/passwd`, a bare `..`) can never survive into the written
/// path: there is nothing left to traverse WITH.
pub fn capture_slug(stem_or_title: &str) -> String {
    capture_slug_checked(stem_or_title).unwrap_or_else(|| "capture".to_string())
}

/// Same policy as [`capture_slug`] (lowercase ASCII-alnum runs joined by
/// `-`, capped at 60 chars) but `None` when the input collapses to empty
/// — no `"capture"` fallback. Desk stable names use this so `".."` /
/// punctuation-only slugs are a typed error rather than silently landing
/// on `capture.md`.
pub fn capture_slug_checked(stem_or_title: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in stem_or_title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let capped: String = out.trim_matches('-').chars().take(60).collect();
    let capped = capped.trim_matches('-').to_string();
    if capped.is_empty() {
        None
    } else {
        Some(capped)
    }
}

/// Title wins (when non-empty); else the ORIGINAL filename's final path
/// component (`Path::file_stem` already discards any leading `../`
/// segments an attacker embedded — defence in depth alongside
/// `capture_slug`'s own separator-stripping); else the `"capture"`
/// fallback.
fn base_stem(title: Option<&str>, original_filename: Option<&str>) -> String {
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        return capture_slug(t);
    }
    if let Some(name) = original_filename {
        let stem = Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        return capture_slug(stem);
    }
    capture_slug("")
}

/// Dot-less lowercase extension for the output filename. Falls back to the
/// pipeline's canonical extension when `ext` is empty or contains anything
/// but ASCII alphanumerics (defence in depth — the caller's `ExtensionMap`
/// already guarantees this shape via `extmap::validate_entry`, but a
/// broken/empty value here would otherwise produce a trailing-dot or
/// unreadable filename).
fn effective_ext(pipeline: Pipeline, ext: &str) -> String {
    let e = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    if e.is_empty() || !e.chars().all(|c| c.is_ascii_alphanumeric()) {
        pipeline_default_ext(pipeline).to_string()
    } else {
        e
    }
}

fn pipeline_default_ext(pipeline: Pipeline) -> &'static str {
    match pipeline {
        Pipeline::Html => "html",
        Pipeline::Markdown => "md",
    }
}

/// Collision-resistant filename: `<base>-<unix_secs>[-n].<ext>`. Mirrors
/// `routes/artifacts.rs`'s memory-ingest collision loop — two same-name
/// captures in the same second land on distinct paths (distinct ids), so
/// nothing silently overwrites.
fn unique_filename(dir: &Path, base: &str, ext: &str, ts: u64) -> String {
    let mut filename = format!("{base}-{ts}.{ext}");
    let mut n = 1;
    while dir.join(&filename).exists() {
        filename = format!("{base}-{ts}-{n}.{ext}");
        n += 1;
    }
    filename
}

// Test-only clock override (see `FrozenNow`) — `None` means "use the real
// wall clock", the production path in every non-test build. Thread-local
// because `cargo test` reuses OS threads across test functions; the guard's
// `Drop` resets it so one test's frozen instant can never leak into another
// sharing the same worker thread.
#[cfg(test)]
thread_local! {
    static TEST_NOW_UNIX: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Freezes [`now_unix`] to a fixed value for the lifetime of the returned
/// guard — the fix for the pre-existing flake in
/// `collision_loop_appends_n_when_same_second`: two real `capture()` calls
/// straddling a wall-clock second boundary would silently stop colliding
/// (different `ts`, no `-1` suffix needed), so the test failed intermittently
/// depending on scheduling. `capture()`'s own behavior is untouched — this
/// only exists under `#[cfg(test)]` and only takes effect when a test
/// explicitly asks for it.
#[cfg(test)]
struct FrozenNow;

#[cfg(test)]
impl FrozenNow {
    fn set(v: u64) -> Self {
        TEST_NOW_UNIX.with(|c| c.set(Some(v)));
        FrozenNow
    }
}

#[cfg(test)]
impl Drop for FrozenNow {
    fn drop(&mut self) {
        TEST_NOW_UNIX.with(|c| c.set(None));
    }
}

fn now_unix() -> u64 {
    #[cfg(test)]
    {
        if let Some(v) = TEST_NOW_UNIX.with(|c| c.get()) {
            return v;
        }
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --- provenance stamping -----------------------------------------------------

/// `source:upload`, `from:<slug>` (when `from` slugifies to non-empty),
/// then every caller tag through `slugify_tag` (the same transform the
/// indexer applies at parse time — a round-trip fixed point per
/// `parser::slugify_tag`'s own doc comment).
fn build_tag_list(from: &str, extra: &[String]) -> Vec<String> {
    let mut tags = vec!["source:upload".to_string()];
    let from_slug = crate::parser::slugify_tag(from);
    if !from_slug.is_empty() {
        tags.push(format!("from:{from_slug}"));
    }
    for t in extra {
        let s = crate::parser::slugify_tag(t);
        if !s.is_empty() {
            tags.push(s);
        }
    }
    tags
}

/// Neutralise a free-text provenance value before it's spliced into flat
/// Markdown front-matter: `markdown::set_frontmatter_field`'s writer is
/// line-oriented (`format!("{key}: {value}")`), so an embedded `\r`/`\n`
/// in an attacker-controlled value (the multipart filename, a shared URL)
/// could inject a bogus extra frontmatter line — or, worse, a line that
/// reads as bare `---` and closes the fence early, splicing arbitrary
/// "frontmatter" into the document. Collapsing every newline to a space
/// means content derived from this value can never equal `---` on its own
/// line, so `parse_frontmatter`'s fence check can't be tricked.
fn sanitize_frontmatter_value(s: &str) -> String {
    s.replace(['\r', '\n'], " ").trim().to_string()
}

/// Optional desk stamps layered onto the historical provenance block.
/// [`StampExtra::default`] is byte-identical to the pre-desk stampers
/// (category `"capture"`, no session, no expiry).
struct StampExtra<'a> {
    session_id: Option<&'a str>,
    expires_at: Option<u64>,
    category: &'a str,
}

impl Default for StampExtra<'static> {
    fn default() -> Self {
        Self {
            session_id: None,
            expires_at: None,
            category: "capture",
        }
    }
}

#[cfg(test)]
/// Stamp provenance into a Markdown capture's front-matter via
/// `markdown::set_frontmatter_field` (creates a fresh `---` block when the
/// source has none). Title is deliberately NOT touched — see
/// [`CaptureInput::title`].
fn stamp_markdown(
    src: &str,
    tags: &[String],
    original_filename: Option<&str>,
    url: Option<&str>,
    ts: u64,
) -> String {
    stamp_markdown_ex(src, tags, original_filename, url, ts, StampExtra::default())
}

fn stamp_markdown_ex(
    src: &str,
    tags: &[String],
    original_filename: Option<&str>,
    url: Option<&str>,
    ts: u64,
    extra: StampExtra<'_>,
) -> String {
    let category_owned = sanitize_frontmatter_value(extra.category);
    let category = if category_owned.is_empty() {
        "capture"
    } else {
        category_owned.as_str()
    };
    let mut out = crate::markdown::set_frontmatter_field(src, "kb-category", Some(category));
    out = crate::markdown::set_frontmatter_field(&out, "kb-tags", Some(&tags.join(", ")));
    if let Some(o) = original_filename
        .map(sanitize_frontmatter_value)
        .filter(|s| !s.is_empty())
    {
        out = crate::markdown::set_frontmatter_field(&out, "kb-capture-original", Some(&o));
    }
    if let Some(u) = url
        .map(sanitize_frontmatter_value)
        .filter(|s| !s.is_empty())
    {
        out = crate::markdown::set_frontmatter_field(&out, "kb-capture-url", Some(&u));
    }
    out = crate::markdown::set_frontmatter_field(&out, "kb-capture-at", Some(&ts.to_string()));
    if let Some(sid) = extra
        .session_id
        .map(sanitize_frontmatter_value)
        .filter(|s| !s.is_empty())
    {
        // Never overwrite an already-present kb-session in the UPLOADED
        // content (the src, not our just-stamped out).
        let (fm, _) = crate::markdown::parse_frontmatter(src);
        if fm.kb_session.is_none() {
            out = crate::markdown::set_frontmatter_field(&out, "kb-session", Some(&sid));
        }
    }
    if let Some(exp) = extra.expires_at {
        out = crate::markdown::set_frontmatter_field(&out, "kb-expires-at", Some(&exp.to_string()));
    }
    out
}

/// HTML-attribute escape for splice-safety. Mirrors `spa.rs`'s
/// `escape_html_attr` (#34) / `meta_edit.rs`'s private `escape_attr`: the
/// minimal set (`& < > "`) needed so an attacker-controlled value can
/// never close the `content="…"` attribute or open a new tag.
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

#[cfg(test)]
/// Stamp provenance `<meta>` tags into an HTML capture's `<head>`. Title
/// is deliberately NOT touched — see [`CaptureInput::title`].
fn stamp_html(
    src: &str,
    tags: &[String],
    original_filename: Option<&str>,
    url: Option<&str>,
    ts: u64,
) -> String {
    stamp_html_ex(src, tags, original_filename, url, ts, StampExtra::default())
}

fn stamp_html_ex(
    src: &str,
    tags: &[String],
    original_filename: Option<&str>,
    url: Option<&str>,
    ts: u64,
    extra: StampExtra<'_>,
) -> String {
    let category = extra.category.trim();
    let category = if category.is_empty() {
        "capture"
    } else {
        category
    };
    let mut metas = String::new();
    metas.push_str(&format!(
        "<meta name=\"kb-category\" content=\"{}\">\n",
        escape_html_attr(category)
    ));
    metas.push_str(&format!(
        "<meta name=\"kb-tags\" content=\"{}\">\n",
        escape_html_attr(&tags.join(", "))
    ));
    if let Some(o) = original_filename.filter(|s| !s.is_empty()) {
        metas.push_str(&format!(
            "<meta name=\"kb-capture-original\" content=\"{}\">\n",
            escape_html_attr(o)
        ));
    }
    if let Some(u) = url.filter(|s| !s.is_empty()) {
        metas.push_str(&format!(
            "<meta name=\"kb-capture-url\" content=\"{}\">\n",
            escape_html_attr(u)
        ));
    }
    metas.push_str(&format!("<meta name=\"kb-capture-at\" content=\"{ts}\">\n"));
    if let Some(sid) = extra.session_id.map(str::trim).filter(|s| !s.is_empty()) {
        // Never overwrite an already-present kb-session in the uploaded
        // content — same never-overwrite rule as stamp_kb_session.
        if crate::parser::extract(src).kb_session.is_none() {
            metas.push_str(&format!(
                "<meta name=\"kb-session\" content=\"{}\">\n",
                escape_html_attr(sid)
            ));
        }
    }
    if let Some(exp) = extra.expires_at {
        metas.push_str(&format!(
            "<meta name=\"kb-expires-at\" content=\"{exp}\">\n"
        ));
    }
    stamp_html_head(src, &metas)
}

/// Splice `metas` immediately before `</head>`. UNLIKE `spa.rs`'s
/// read-only `inject_head_meta` (#34, a no-op when `</head>` is absent —
/// it only ever touches an existing served artifact's shell), an uploaded
/// capture may have NO head at all: a bare HTML fragment, or plain text
/// saved with a `.html` extension. In that case synthesize a minimal
/// `<head>…</head>` at the very top of the document, so provenance is
/// never silently dropped. `scraper`/html5ever's document-parsing
/// algorithm (used by `parser::extract`) tolerates a leading `<head>`
/// with no wrapping `<html>`/`<body>`, so the synthesized block still
/// parses as real head metadata.
fn stamp_html_head(html: &str, metas: &str) -> String {
    match html.find("</head>") {
        Some(idx) => {
            let mut out = String::with_capacity(html.len() + metas.len());
            out.push_str(&html[..idx]);
            out.push_str(metas);
            out.push_str(&html[idx..]);
            out
        }
        None => format!("<head>\n{metas}</head>\n{html}"),
    }
}

// --- sanitize (Html only) ----------------------------------------------------

/// Opt-in HTML sanitize: ammonia defaults + `id` added to the generic
/// (any-tag) attribute allowlist, so section anchors survive, + `title`
/// added to the CLEAN-CONTENT blacklist (`add_clean_content_tags`) so a
/// real `<title>` element's TEXT is dropped wholesale rather than
/// leaking as loose text (ammonia's default treatment of a disallowed,
/// non-clean-content tag is to UNWRAP it — drop just the tag, keep the
/// inner text — which is exactly wrong for `<title>`: the page's title
/// would otherwise surface as orphaned text glued in front of the
/// body). Ammonia's defaults already strip `<script>`/`<style>`
/// CONTENTS and every `on*=` handler (they're simply not in the
/// allowed-attribute set).
///
/// Ammonia has no html/head/title/body/meta allowlist entries at all —
/// by design it cleans as a BODY-CONTEXT FRAGMENT, so feeding it a
/// realistic full saved page loses `<!doctype>`/`<html>`/`<head>`/
/// `<meta charset>` wholesale, and (absent the `add_clean_content_tags`
/// above) the `<title>` ELEMENT along with it — the title TEXT would
/// otherwise leak as orphaned loose text in front of the body. This
/// function recovers the original title BEFORE cleaning (cleaning would
/// otherwise destroy it) via [`extract_title`], then reassembles a
/// minimal, valid document envelope around the cleaned fragment with
/// the title re-attached, escaped, in a real `<title>` — so a sanitized
/// capture is a real document, not headless soup. When no title was
/// found, the `<title>` element is omitted entirely (the indexer's
/// `title.or(h1).or(stem)` fallback — mirrored by [`resolved_title`] —
/// covers it). Capture-time transform — the stored source IS the
/// sanitized output.
fn sanitize_html(html: &str) -> String {
    let title = extract_title(html);
    let fragment = ammonia::Builder::default()
        .add_generic_attributes(["id"])
        .add_clean_content_tags(["title"])
        .clean(html)
        .to_string();
    let title_tag = match title {
        Some(t) => format!("<title>{}</title>", escape_html_attr(&t)),
        None => String::new(),
    };
    format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\">{title_tag}</head><body>{fragment}</body></html>"
    )
}

/// Extract the original `<title>` element's TEXT, case-insensitively,
/// tolerating attributes on the opening tag (`<title lang="en">`). A
/// byte-oriented, RCDATA-faithful scan: the first literal `</title`
/// (case-insensitive, no entity awareness — matching how a real HTML
/// tokenizer closes the element) always ends it, so a title containing
/// entity-encoded `&lt;/title&gt;` text round-trips as plain text while
/// a genuinely early close (`<title></title><script>…`) yields an EMPTY
/// title — same as a spec-compliant parser would see — rather than
/// swallowing the trailing script text into the "title". Common
/// named/numeric entities are decoded via [`html_unescape`] so the
/// result is plain text, ready for [`escape_html_attr`] on the way back
/// out through [`sanitize_html`]. `None` when no title element is found
/// or its text is empty after decode+trim.
fn extract_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let open = lower.find("<title")?;
    let after_name = open + "<title".len();
    match html[after_name..].chars().next() {
        Some(c) if c == '>' || c.is_whitespace() => {}
        _ => return None, // e.g. `<titlebar>` — not actually a title tag
    }
    let close_gt = html[after_name..].find('>')? + after_name;
    let content_start = close_gt + 1;
    let rel_end = lower[content_start..].find("</title")?;
    let content_end = content_start + rel_end;
    let text = html_unescape(html[content_start..content_end].trim());
    (!text.is_empty()).then_some(text)
}

/// Decode the small set of named/numeric entities a `<title>`'s text
/// realistically carries (mirrors `session_render::html_unescape`) —
/// good enough for round-tripping title text through
/// [`extract_title`]/[`sanitize_html`]; not a general HTML entity
/// decoder.
fn html_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

// --- response title -----------------------------------------------------

/// Best-effort title for the response, mirroring the indexer's own
/// `fields.title.or(fields.h1).unwrap_or_else(filename-stem)` fallback
/// chain (`indexer::prepare_doc`) so the response never disagrees with
/// what shows up once the watcher indexes the file.
fn resolved_title(pipeline: Pipeline, stamped_src: &str, filename: &str) -> String {
    let (title, h1) = match pipeline {
        Pipeline::Html => {
            let f = crate::parser::extract(stamped_src);
            (f.title, f.h1)
        }
        Pipeline::Markdown => {
            let (f, _) = crate::parser::extract_markdown(stamped_src);
            (f.title, f.h1)
        }
    };
    title.or(h1).unwrap_or_else(|| {
        Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("untitled")
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        (tmp, root)
    }

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn md_input<'a>(root: &'a Path, bytes: &'a [u8], t: &'a [String]) -> CaptureInput<'a> {
        CaptureInput {
            source_root: root,
            capture_dir: "capture",
            pipeline: Pipeline::Markdown,
            ext: "md",
            bytes,
            original_filename: None,
            title: Some("Desk Draft"),
            tags: t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        }
    }

    // --- filename policy: collision + traversal -----------------------------

    #[test]
    fn collision_loop_appends_n_when_same_second() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        // Freeze the clock so both captures land on the SAME `ts` regardless
        // of scheduling — deterministic per-test, `capture()` itself is
        // unmodified (`now_unix` only reads `TEST_NOW_UNIX` under `cfg(test)`,
        // and only when a test has explicitly set it).
        let _frozen = FrozenNow::set(1_700_000_000);
        let a = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Markdown,
            ext: "md",
            bytes: b"# One",
            original_filename: None,
            title: Some("Same Title"),
            tags: &t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        })
        .unwrap();
        let b = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Markdown,
            ext: "md",
            bytes: b"# Two",
            original_filename: None,
            title: Some("Same Title"),
            tags: &t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        })
        .unwrap();
        assert_ne!(a.source_relative, b.source_relative);
        assert_ne!(a.id, b.id);
        assert!(b.source_relative.contains("-1.md"), "{}", b.source_relative);
    }

    #[test]
    fn original_filename_traversal_cannot_escape_capture_dir() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let out = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Markdown,
            ext: "md",
            bytes: b"body",
            original_filename: Some("../../../../etc/passwd"),
            title: None,
            tags: &t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        })
        .unwrap();
        // The written path must sit under `<root>/capture/`, never escape it,
        // and must be exactly ONE path segment below it (no nested traversal).
        let filename = out
            .source_relative
            .strip_prefix("capture/")
            .expect("must sit directly under capture/");
        assert!(!filename.contains('/'), "{filename}");
        assert!(!out.source_relative.contains(".."));
        let abs = root.join(&out.source_relative);
        assert!(abs.starts_with(root.join("capture")));
        assert!(abs.exists());
    }

    #[test]
    fn capture_slug_strips_separators_and_dots() {
        assert_eq!(capture_slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(capture_slug(".."), "capture");
        assert_eq!(capture_slug("a/b\\c"), "a-b-c");
        assert_eq!(capture_slug(""), "capture");
        assert_eq!(capture_slug("Hello, World!"), "hello-world");
        assert_eq!(capture_slug_checked(".."), None);
        assert_eq!(capture_slug_checked(""), None);
        assert_eq!(capture_slug_checked("!!!"), None);
        assert_eq!(
            capture_slug_checked("Hello, World!").as_deref(),
            Some("hello-world")
        );
        assert_eq!(capture_slug_checked("capture").as_deref(), Some("capture"));
    }

    // --- markdown stamping golden --------------------------------------------

    #[test]
    fn md_stamping_golden() {
        let out = stamp_markdown(
            "# My Notes\n\nBody text.\n",
            &tags(&["source:upload", "from:cli", "project"]),
            Some("notes.md"),
            Some("https://example.com/page"),
            1_700_000_000,
        );
        assert!(out.starts_with("---\n"), "{out}");
        assert!(out.contains("kb-category: capture\n"), "{out}");
        assert!(
            out.contains("kb-tags: source:upload, from:cli, project\n"),
            "{out}"
        );
        assert!(out.contains("kb-capture-original: notes.md\n"), "{out}");
        assert!(
            out.contains("kb-capture-url: https://example.com/page\n"),
            "{out}"
        );
        assert!(out.contains("kb-capture-at: 1700000000\n"), "{out}");
        // Body untouched, verbatim after the frontmatter block.
        assert!(out.trim_end().ends_with("Body text."));
        // Round-trips through the parser cleanly.
        let (fm, body) = crate::markdown::parse_frontmatter(&out);
        assert_eq!(fm.kb_category.as_deref(), Some("capture"));
        assert_eq!(body.trim(), "# My Notes\n\nBody text.".trim());
    }

    #[test]
    fn md_stamping_preserves_existing_frontmatter_and_title() {
        let src = "---\ntitle: Already Titled\ncustom-key: keep-me\n---\nBody.\n";
        let out = stamp_markdown(src, &tags(&["source:upload"]), None, None, 42);
        assert!(out.contains("title: Already Titled"), "{out}");
        assert!(out.contains("custom-key: keep-me"), "{out}");
        assert!(out.contains("kb-category: capture"), "{out}");
        // No original/url keys stamped when absent.
        assert!(!out.contains("kb-capture-original"));
        assert!(!out.contains("kb-capture-url"));
    }

    #[test]
    fn md_frontmatter_injection_via_newline_is_neutralised() {
        // An attacker-controlled filename/url containing an embedded
        // `---` line must not be able to smuggle new frontmatter keys or
        // close the fence early.
        let evil = "evil\n---\nkb-category: memory-user\n---\nsmuggled";
        let out = stamp_markdown("body", &tags(&["source:upload"]), Some(evil), None, 1);
        let (fm, _) = crate::markdown::parse_frontmatter(&out);
        // The category stamped by capture wins — never overridden by the
        // smuggled value.
        assert_eq!(fm.kb_category.as_deref(), Some("capture"));
        // The smuggled line survived only as inert single-line text.
        assert!(
            out.contains("kb-capture-original: evil --- kb-category: memory-user --- smuggled")
                || out.contains("kb-capture-original: evil")
        );
        assert!(!out.contains("\n---\nkb-category: memory-user\n---\n"));
    }

    // --- html stamping golden + XSS -------------------------------------------

    #[test]
    fn html_stamping_golden() {
        let src = "<!doctype html><html><head><title>Hi</title></head><body>x</body></html>";
        let out = stamp_html(
            src,
            &tags(&["source:upload", "from:spa"]),
            Some("page.html"),
            Some("https://example.com"),
            1_700_000_000,
        );
        assert!(out.find("kb-category").unwrap() < out.find("</head>").unwrap());
        assert!(out.contains(r#"<meta name="kb-category" content="capture">"#));
        assert!(out.contains(r#"<meta name="kb-tags" content="source:upload, from:spa">"#));
        assert!(out.contains(r#"<meta name="kb-capture-original" content="page.html">"#));
        assert!(out.contains(r#"<meta name="kb-capture-url" content="https://example.com">"#));
        assert!(out.contains(r#"<meta name="kb-capture-at" content="1700000000">"#));
        // Original title/body untouched.
        assert!(out.contains("<title>Hi</title>"));
        assert!(out.contains("<body>x</body>"));
    }

    #[test]
    fn html_stamping_escapes_xss_in_original_filename_and_url() {
        let src = "<html><head></head><body></body></html>";
        let evil_name = "\"><script>alert(1)</script>";
        let evil_url = "\" onmouseover=\"alert(1)";
        let out = stamp_html(src, &tags(&[]), Some(evil_name), Some(evil_url), 1);
        assert!(!out.contains("<script>alert(1)</script>"), "{out}");
        assert!(!out.contains("onmouseover=\"alert(1)"), "{out}");
        assert!(out.contains("&quot;&gt;&lt;script&gt;"), "{out}");
        assert!(out.contains("&quot; onmouseover=&quot;alert(1)"), "{out}");
    }

    #[test]
    fn headless_html_gets_a_synthesized_head() {
        let src = "<p>just a fragment, no head at all</p>";
        let out = stamp_html(src, &tags(&["source:upload"]), None, None, 1);
        assert!(out.starts_with("<head>\n"), "{out}");
        assert!(out.contains("</head>\n<p>just a fragment"), "{out}");
        assert!(out.contains(r#"<meta name="kb-category" content="capture">"#));
        // The synthesized head parses: `parser::extract` still finds the
        // kb-category meta via a full document parse.
        let fields = crate::parser::extract(&out);
        assert_eq!(fields.kb_category.as_deref(), Some("capture"));
    }

    #[test]
    fn plain_text_saved_as_html_is_handled() {
        // No tags, no structure at all — the common "shared a text snippet
        // as .html" case.
        let out = stamp_html("just plain text, no html tags", &tags(&[]), None, None, 5);
        assert!(out.contains(r#"<meta name="kb-capture-at" content="5">"#));
        assert!(out.ends_with("just plain text, no html tags"));
    }

    // --- title extraction (pre-sanitize) ----------------------------------------

    #[test]
    fn extract_title_plain() {
        let src = "<html><head><title>My Great Page</title></head><body></body></html>";
        assert_eq!(extract_title(src).as_deref(), Some("My Great Page"));
    }

    #[test]
    fn extract_title_tolerates_attributes_and_case() {
        let src = r#"<HTML><HEAD><TiTlE lang="en" class="x">Hi There</TiTlE></HEAD></HTML>"#;
        assert_eq!(extract_title(src).as_deref(), Some("Hi There"));
    }

    #[test]
    fn extract_title_collapses_via_trim_and_decodes_entities() {
        let src = "<title>  A &amp; B  </title>";
        assert_eq!(extract_title(src).as_deref(), Some("A & B"));
    }

    #[test]
    fn extract_title_none_when_absent_or_empty() {
        assert_eq!(
            extract_title("<html><body>no title here</body></html>"),
            None
        );
        assert_eq!(extract_title("<title>   </title>"), None);
        assert_eq!(extract_title("<titlebar>not a title tag</titlebar>"), None);
    }

    #[test]
    fn extract_title_empty_when_closed_immediately() {
        // Mirrors real HTML tokenizing: the FIRST literal `</title` always
        // closes the element, so a script placed right after an empty
        // title never gets folded into "the title".
        let src = "<title></title><script>alert(1)</script>";
        assert_eq!(extract_title(src), None);
    }

    // --- sanitize pins ---------------------------------------------------------

    #[test]
    fn sanitize_strips_script_and_onclick_but_keeps_id() {
        let dirty = r#"<div id="s1" onclick="evil()"><script>alert('xss')</script>hello</div>"#;
        let clean = sanitize_html(dirty);
        assert!(!clean.contains("<script"), "{clean}");
        assert!(!clean.contains("alert"), "{clean}");
        assert!(!clean.contains("onclick"), "{clean}");
        assert!(clean.contains(r#"id="s1""#), "{clean}");
        assert!(clean.contains("hello"), "{clean}");
        // U1 follow-up: no source title here, so the envelope carries no
        // `<title>` at all — but it IS a real envelope now, not a bare
        // fragment.
        assert!(clean.starts_with("<!doctype html>"), "{clean}");
        assert!(!clean.contains("<title"), "{clean}");
    }

    #[test]
    fn sanitize_full_page_reassembles_envelope_with_original_title() {
        let src = concat!(
            "<!doctype html><html><head><meta charset=\"utf-8\">",
            "<title>My Great Page</title></head><body>",
            "<p onclick=\"evil()\">hi</p><script>bad()</script>",
            "</body></html>",
        );
        let out = sanitize_html(src);
        assert!(out.starts_with("<!doctype html>"), "{out}");
        assert_eq!(out.matches("<title").count(), 1, "{out}");
        assert!(out.contains("<title>My Great Page</title>"), "{out}");
        assert!(!out.contains("<script"), "{out}");
        assert!(!out.contains("onclick"), "{out}");
        // No duplicate loose copy of the title text floating in the body —
        // ammonia's `add_clean_content_tags(["title"])` drops the ENTIRE
        // `<title>…</title>` subtree from the cleaned fragment, so the
        // only occurrence of the title text is the one we reattach.
        let body_start = out.find("<body>").expect("has a body");
        assert!(!out[body_start..].contains("My Great Page"), "{out}");
    }

    #[test]
    fn sanitize_hostile_title_is_escaped_and_inert() {
        // The title's own TEXT is the entity-encoded form of
        // `</title><script>alert(1)</script>` — a perfectly valid `<title>`
        // whose DECODED text happens to look like markup. Must round-trip
        // as inert text, never as a live tag.
        let src = concat!(
            "<html><head><title>",
            "&lt;/title&gt;&lt;script&gt;alert(1)&lt;/script&gt;",
            "</title></head><body></body></html>",
        );
        let out = sanitize_html(src);
        assert!(!out.contains("<script>alert(1)</script>"), "{out}");
        assert!(
            out.contains("&lt;/title&gt;&lt;script&gt;alert(1)&lt;/script&gt;"),
            "{out}"
        );
        assert_eq!(out.matches("<title").count(), 1, "{out}");
    }

    #[test]
    fn sanitize_malformed_early_close_title_stays_inert() {
        // A `<title>` closed immediately, followed by a real `<script>`
        // sibling — exactly how a spec HTML parser tokenizes this (EMPTY
        // title, live script sibling). The script text must never end up
        // captured as "the title".
        let src = "<html><head><title></title><script>alert(1)</script></head><body></body></html>";
        let out = sanitize_html(src);
        assert!(!out.contains("<script"), "{out}");
        assert!(!out.contains("alert(1)"), "{out}");
        assert!(!out.contains("<title>"), "{out}");
    }

    #[test]
    fn sanitize_headless_fragment_gets_a_valid_envelope() {
        let src = "<p>just a fragment, no head or html tags at all</p>";
        let out = sanitize_html(src);
        assert!(out.starts_with("<!doctype html>"), "{out}");
        assert!(
            out.contains("<head><meta charset=\"utf-8\"></head>"),
            "{out}"
        );
        assert!(
            out.contains("<body><p>just a fragment, no head or html tags at all</p></body>"),
            "{out}"
        );
    }

    #[test]
    fn capture_with_sanitize_true_produces_script_free_output() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let out = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Html,
            ext: "html",
            bytes: br#"<html><body><img src=x onerror="alert(1)"><script>bad()</script>ok</body></html>"#,
            original_filename: Some("evil.html"),
            title: None,
            tags: &t,
            from: "share",
            sanitize: true,
            url: None,
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        })
        .unwrap();
        let abs = root.join(&out.source_relative);
        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(!written.contains("<script"), "{written}");
        assert!(!written.contains("onerror"), "{written}");
        assert!(written.contains("kb-category"));
        // U1 follow-up: the stored source is now a real document, not a
        // headless fragment.
        assert!(written.starts_with("<!doctype html>"), "{written}");
    }

    #[test]
    fn capture_with_sanitize_false_leaves_script_intact() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let out = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Html,
            ext: "html",
            bytes: b"<html><body><script>keep()</script></body></html>",
            original_filename: Some("page.html"),
            title: None,
            tags: &t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        })
        .unwrap();
        let abs = root.join(&out.source_relative);
        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(written.contains("<script>keep()</script>"), "{written}");
    }

    #[test]
    fn capture_sanitized_html_keeps_envelope_and_provenance_metas_together() {
        // Point 4 of the U1 follow-up: sanitize MUST run before provenance
        // stamping, so the stamped `<meta>`s land inside (and survive in)
        // the envelope `sanitize_html` builds. If stamping ran first (on
        // the raw upload, before any `</head>` existed) and sanitize ran
        // second, ammonia's fragment-cleaning would have dropped those
        // metas right along with the rest of the head.
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let out = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Html,
            ext: "html",
            bytes: br#"<!doctype html><html><head><title>Saved Page</title></head><body><script>bad()</script><p>keep me</p></body></html>"#,
            original_filename: Some("saved.html"),
            title: None,
            tags: &t,
            from: "share",
            sanitize: true,
            url: Some("https://example.com/x"),
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        })
        .unwrap();
        let abs = root.join(&out.source_relative);
        let written = std::fs::read_to_string(&abs).unwrap();
        // The envelope survived sanitize, title intact.
        assert!(written.starts_with("<!doctype html>"), "{written}");
        assert!(written.contains("<title>Saved Page</title>"), "{written}");
        // The provenance metas landed INSIDE that same head.
        assert!(
            written.contains(r#"<meta name="kb-category" content="capture">"#),
            "{written}"
        );
        assert!(written.contains("kb-capture-url"), "{written}");
        assert!(written.find("kb-category").unwrap() < written.find("</head>").unwrap());
        assert!(!written.contains("<script"), "{written}");
        assert!(written.contains("keep me"), "{written}");
        // The response title resolves through the real parser now that a
        // real `<title>` element survives sanitize.
        assert_eq!(out.title, "Saved Page");
    }

    // --- url/text stub golden ---------------------------------------------------

    #[test]
    fn url_stub_golden() {
        let body = build_url_stub(
            "Great Article",
            Some("https://example.com/a"),
            Some("shared commentary text"),
        );
        assert_eq!(
            body,
            "# Great Article\n\n<https://example.com/a>\n\nshared commentary text\n"
        );
    }

    #[test]
    fn url_stub_omits_absent_parts() {
        let title_only = build_url_stub("Just A Title", None, None);
        assert_eq!(title_only, "# Just A Title\n");
    }

    #[test]
    fn capture_url_stub_writes_md_with_kind_tag_and_url_meta() {
        let (_tmp, root) = ctx();
        let t = tags(&["work"]);
        let out = capture_url_stub(UrlStubInput {
            source_root: &root,
            capture_dir: "capture",
            title: Some("Shared Page"),
            url: Some("https://example.com/shared"),
            text: Some("worth reading"),
            tags: &t,
            from: "share",
        })
        .unwrap();
        assert!(
            out.source_relative.ends_with(".md"),
            "{}",
            out.source_relative
        );
        assert_eq!(out.title, "Shared Page");
        let abs = root.join(&out.source_relative);
        let written = std::fs::read_to_string(&abs).unwrap();
        assert!(
            written.contains("kb-tags: source:upload, from:share, work, kind:url-stub"),
            "{written}"
        );
        assert!(
            written.contains("kb-capture-url: https://example.com/shared"),
            "{written}"
        );
        assert!(written.contains("# Shared Page"));
        assert!(written.contains("worth reading"));
    }

    #[test]
    fn capture_url_stub_defaults_title_when_absent() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let out = capture_url_stub(UrlStubInput {
            source_root: &root,
            capture_dir: "capture",
            title: None,
            url: Some("https://example.com/x"),
            text: None,
            tags: &t,
            from: "cli",
        })
        .unwrap();
        assert_eq!(out.title, "Untitled capture");
    }

    // --- id ≡ indexer derivation --------------------------------------------

    #[test]
    fn returned_id_matches_the_indexer_derivation() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let out = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Markdown,
            ext: "md",
            bytes: b"# Doc",
            original_filename: None,
            title: Some("A Title"),
            tags: &t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: None,
            session_id: None,
            expires_at: None,
            category: None,
        })
        .unwrap();
        // This IS `indexer::identity_for`'s own formula — the shared
        // #27 derivation: `ArtifactId::from_path(doc_rel_path(abs, root))`.
        let abs = root.join(&out.source_relative);
        let rel = crate::paths::doc_rel_path(&abs.to_string_lossy(), &root);
        let expected = ArtifactId::from_path(&rel).to_string();
        assert_eq!(out.id, expected);
        assert_eq!(rel, out.source_relative);
    }

    #[test]
    fn id_is_stable_across_a_capture_with_different_bytes_same_name() {
        // #27: identity follows the PATH, not the content-hash.
        let a = ArtifactId::from_path("capture/foo-1.md");
        let b_same_path_diff_bytes = ArtifactId::from_path("capture/foo-1.md");
        assert_eq!(a.as_str(), b_same_path_diff_bytes.as_str());
    }

    // --- config resolution ---------------------------------------------------

    #[test]
    fn effective_ext_falls_back_on_garbage() {
        assert_eq!(effective_ext(Pipeline::Html, "html"), "html");
        assert_eq!(effective_ext(Pipeline::Markdown, "TXT"), "txt");
        assert_eq!(effective_ext(Pipeline::Html, ""), "html");
        assert_eq!(effective_ext(Pipeline::Markdown, ".."), "md");
        assert_eq!(effective_ext(Pipeline::Html, "html/evil"), "html");
    }

    // --- desk: stable_name / session / expires ------------------------------

    #[test]
    fn stable_name_overwrites_same_path_same_id() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let _frozen = FrozenNow::set(1_700_000_000);
        let mut input = md_input(&root, b"# First", &t);
        input.stable_name = Some("brief");
        let a = capture(input).unwrap();
        assert!(a.created);
        assert_eq!(a.source_relative, "capture/brief.md");
        assert!(!a.source_relative.contains("1700000000"));

        let mut input = md_input(&root, b"# Second", &t);
        input.stable_name = Some("brief");
        let b = capture(input).unwrap();
        assert!(!b.created, "overwrite must report created=false");
        assert_eq!(a.source_relative, b.source_relative);
        assert_eq!(a.id, b.id);
        let written = std::fs::read_to_string(root.join(&b.source_relative)).unwrap();
        assert!(written.contains("# Second"), "{written}");
        assert!(!written.contains("# First"), "{written}");
    }

    #[test]
    fn stable_name_empty_after_slug_is_bad_request() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let mut input = md_input(&root, b"body", &t);
        input.stable_name = Some("..");
        let err = capture(input).unwrap_err();
        match err {
            Error::BadRequest(msg) => assert!(msg.contains("slugified to empty"), "{msg}"),
            other => panic!("expected BadRequest, got {other:?}"),
        }
        assert!(!root.join("capture").join("capture.md").exists());
    }

    #[test]
    fn absent_stable_name_session_expires_are_byte_identical_to_unique_path() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let _frozen = FrozenNow::set(1_700_000_000);
        let out = capture(md_input(&root, b"# Pin", &t)).unwrap();
        assert!(out.created);
        assert!(
            out.source_relative.contains("-1700000000.md"),
            "{}",
            out.source_relative
        );
        let written = std::fs::read_to_string(root.join(&out.source_relative)).unwrap();
        assert!(written.contains("kb-category: capture\n"), "{written}");
        assert!(!written.contains("kb-session"), "{written}");
        assert!(!written.contains("kb-expires-at"), "{written}");
        assert!(!written.contains("handoff"), "{written}");
    }

    #[test]
    fn session_id_stamps_markdown_and_parser_reads_it() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let mut input = md_input(&root, b"# Body\n", &t);
        input.stable_name = Some("sess-md");
        input.session_id = Some("sess-abc-123");
        let out = capture(input).unwrap();
        let written = std::fs::read_to_string(root.join(&out.source_relative)).unwrap();
        assert!(written.contains("kb-session: sess-abc-123\n"), "{written}");
        let (fields, _) = crate::parser::extract_markdown(&written);
        assert_eq!(fields.kb_session.as_deref(), Some("sess-abc-123"));
    }

    #[test]
    fn session_id_never_overwrites_existing_markdown_kb_session() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let src = "---\nkb-session: already-set\n---\nbody\n";
        let mut input = md_input(&root, src.as_bytes(), &t);
        input.stable_name = Some("keep-sess");
        input.session_id = Some("sess-new");
        let out = capture(input).unwrap();
        let written = std::fs::read_to_string(root.join(&out.source_relative)).unwrap();
        let (fm, _) = crate::markdown::parse_frontmatter(&written);
        assert_eq!(fm.kb_session.as_deref(), Some("already-set"));
        assert!(!written.contains("sess-new"), "{written}");
    }

    #[test]
    fn session_id_stamps_html_and_parser_reads_it() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let src = "<html><head><title>Hi</title></head><body>x</body></html>";
        let out = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Html,
            ext: "html",
            bytes: src.as_bytes(),
            original_filename: Some("hi.html"),
            title: None,
            tags: &t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: Some("sess-html"),
            session_id: Some("sess-html-1"),
            expires_at: None,
            category: None,
        })
        .unwrap();
        let written = std::fs::read_to_string(root.join(&out.source_relative)).unwrap();
        assert!(
            written.contains(r#"<meta name="kb-session" content="sess-html-1">"#),
            "{written}"
        );
        let fields = crate::parser::extract(&written);
        assert_eq!(fields.kb_session.as_deref(), Some("sess-html-1"));
    }

    #[test]
    fn session_id_never_overwrites_existing_html_kb_session() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let src = r#"<html><head><meta name="kb-session" content="already-set"></head><body>x</body></html>"#;
        let out = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Html,
            ext: "html",
            bytes: src.as_bytes(),
            original_filename: Some("hi.html"),
            title: None,
            tags: &t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: Some("keep-html-sess"),
            session_id: Some("sess-new"),
            expires_at: None,
            category: None,
        })
        .unwrap();
        let written = std::fs::read_to_string(root.join(&out.source_relative)).unwrap();
        let fields = crate::parser::extract(&written);
        assert_eq!(fields.kb_session.as_deref(), Some("already-set"));
        assert!(!written.contains("sess-new"), "{written}");
    }

    #[test]
    fn expires_at_stamps_markdown_and_html() {
        let (_tmp, root) = ctx();
        let t = tags(&[]);
        let mut input = md_input(&root, b"# ttl\n", &t);
        input.stable_name = Some("ttl-md");
        input.expires_at = Some(1_800_000_000);
        let out = capture(input).unwrap();
        let written = std::fs::read_to_string(root.join(&out.source_relative)).unwrap();
        assert!(written.contains("kb-expires-at: 1800000000\n"), "{written}");

        let html = "<html><head></head><body>x</body></html>";
        let out = capture(CaptureInput {
            source_root: &root,
            capture_dir: "capture",
            pipeline: Pipeline::Html,
            ext: "html",
            bytes: html.as_bytes(),
            original_filename: Some("ttl.html"),
            title: None,
            tags: &t,
            from: "cli",
            sanitize: false,
            url: None,
            stable_name: Some("ttl-html"),
            session_id: None,
            expires_at: Some(1_800_000_000),
            category: None,
        })
        .unwrap();
        let written = std::fs::read_to_string(root.join(&out.source_relative)).unwrap();
        assert!(
            written.contains(r#"<meta name="kb-expires-at" content="1800000000">"#),
            "{written}"
        );
    }

    #[test]
    fn category_handoff_stamps_instead_of_capture() {
        let (_tmp, root) = ctx();
        let t = tags(&["draft"]);
        let mut input = md_input(&root, b"# Handoff\n", &t);
        input.stable_name = Some("ticket");
        input.capture_dir = DESK_DIR;
        input.category = Some("handoff");
        let out = capture(input).unwrap();
        assert_eq!(out.source_relative, "handoff/ticket.md");
        let written = std::fs::read_to_string(root.join(&out.source_relative)).unwrap();
        assert!(written.contains("kb-category: handoff\n"), "{written}");
        assert!(!written.contains("kb-category: capture\n"), "{written}");
        assert!(written.contains("draft"), "{written}");
    }
}
