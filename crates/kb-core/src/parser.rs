//! HTML field extractor. Lifted from `spike-walker` (six base fields:
//! title / h1 / p / svg_count / has_details / has_script — confirmed
//! by topic 07 §D and the spike-walker findings) and extended with the
//! nine production facets the `kb-core::storage::schema` `Doc` requires:
//!
//! - `has_form`, `has_canvas`, `has_animation`, `has_math`, `has_drag`
//! - `kb_category` (from `<meta name="kb-category">`)
//! - `prompt` + `prompt_size_bytes` (from `<template id="kb-prompt">`)
//! - `js_loc` / `css_loc` bucketed (per topic 07's static / light /
//!   interactive / rich classification)
//! - `body_text_excerpt` (~400 char depth-first walk skipping
//!   `<script>/<style>/<template>` — the SVG-heavy fallback the
//!   spike-walker findings surfaced)
//! - `headings` + `code` + `body` (full text, for FTS)
//!
//! All CSS selectors are parsed once via a `OnceLock<ParserSet>` cache —
//! without this, a 50k-doc reindex re-parses ~700k selectors. Topic 07 says
//! the parser must stay well under 5 ms/file; spike-walker measured 159 µs
//! mean, so additional selectors here are within budget.

use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const BODY_EXCERPT_MAX_CHARS: usize = 400;
const PROMPT_MAX_BYTES: usize = 8 * 1024; // topic 02 §Decisions: 8 KB cap

/// All fields a single HTML document yields. The indexer combines this with
/// path/mtime/size_kb metadata to build a `storage::schema::Doc`.
// Note: NOT `Eq` — `kb_salience: Option<f32>` (f32 is not `Eq`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Fields {
    // --- 6 base fields (spike-walker, confirmed) -----------------------------
    pub title: Option<String>,
    pub h1: Option<String>,
    pub p: Option<String>,
    pub svg_count: u32,
    pub has_details: bool,
    pub has_script: bool,

    // --- 9 facets (topic 07 §D extension) ------------------------------------
    pub has_form: bool,
    pub has_canvas: bool,
    pub has_animation: bool,
    pub has_math: bool,
    pub has_drag: bool,
    pub kb_category: Option<String>,
    pub prompt: Option<String>,
    pub prompt_size_bytes: u32,
    pub js_loc: LocBucket,
    pub css_loc: LocBucket,

    /// v0.6 T1 — explicit tags from `<meta name="kb-tags">` (comma-
    /// separated content). Empty if the artifact has no meta tag —
    /// the indexer falls back to path-derived tags before building
    /// the storage `Doc`.
    pub tags: Vec<String>,

    /// v0.7 S1 — explicit status from `<meta name="kb-status">`
    /// (e.g. "open", "applied", "draft", "shipped"). Free-form string;
    /// kb does not enforce a vocabulary. Empty when the meta tag is
    /// absent. Surfaced as a gallery filter facet alongside kb-category.
    pub kb_status: Option<String>,

    /// v0.7 S1 — explicit severity from `<meta name="kb-severity">`
    /// (e.g. "low", "medium", "high", "critical", or scheme-specific
    /// "sev-1".."sev-4"). Free-form string. Empty when the meta tag is
    /// absent. Pairs with kb_status for incident/fix-style artifacts.
    pub kb_severity: Option<String>,

    /// v0.9 M1 — importance weight from `<meta name="kb-salience">`,
    /// an f32 clamped to 0..1. Drives recall ranking. None when the
    /// meta tag is absent or its content does not parse as a float.
    pub kb_salience: Option<f32>,

    /// v0.9 M1 — recency fade rate from `<meta name="kb-decay">`
    /// ("slow" | "fast"). Free-form; recall treats an absent/unknown
    /// value as "slow". None when the meta tag is absent.
    pub kb_decay: Option<String>,

    /// v0.9 M1 — artifact id this memory supersedes, from
    /// `<meta name="kb-supersedes">`. Recall drops the named id from
    /// results. None when the meta tag is absent.
    pub kb_supersedes: Option<String>,

    /// v0.14 S1 — id of the Claude Code conversation session that
    /// produced this memory, from `<meta name="kb-session">`. Set by
    /// `kb remember` when the marker file
    /// `~/.cache/kb/current-session` is present, or by `kb-capture.sh`
    /// when wrapping a Stop-hook transcript. None when the meta tag is
    /// absent. Surfaced as the `kb_session` lance projection column so
    /// the /api/sessions/{sid}/memories endpoint can filter on it.
    pub kb_session: Option<String>,

    /// RA3 — write-time creation timestamp (unix seconds) from
    /// `<meta name="kb-created">`, stamped by the ingest route. Overrides
    /// the filesystem-btime `created_unix` for memories so the recall decay
    /// basis stays stable across reindex (unlike mtime/btime). None when the
    /// meta is absent or unparseable.
    pub kb_created: Option<i64>,

    /// RA4 — one-line memory summary distinct from the title, from
    /// `<meta name="kb-summary">`. Surfaced on recall hits as a clean gloss.
    /// None when absent.
    pub kb_summary: Option<String>,

    /// MI-W3.3a — optional CoALA-minimal classification from
    /// `<meta name="kb-memory-type">` (`episodic` | `semantic` |
    /// `procedural`). Stored as a free-form string here (validated against
    /// the closed set at the WRITE boundary — `kb_core::memory::MemoryType`
    /// — same precedent as `kb_decay`). None when absent; kb never infers
    /// or backfills a type onto an existing memory.
    pub kb_memory_type: Option<String>,

    /// MI-W3.4 — write-time trust tag from `<meta name="kb-source">`
    /// (`fetched-web` | `user-dictated` | `agent-inference`). Same
    /// free-form-string-at-this-layer precedent as `kb_memory_type`
    /// (`kb_core::memory::TrustSource` validates at the write boundary).
    /// SURFACED (census/recall), NEVER SCORED.
    pub kb_source: Option<String>,

    /// CT-A1 (U3 parse-back) — the `you`/`claude` role from `<meta
    /// name="kb-author">`. Distinct from `kb_source` above (MI-W3.4's
    /// write-time TRUST tag is a different meta entirely — `kb-source` vs
    /// `kb-source-kb`/`kb-source-artifact`/`kb-source-anchor` below). None
    /// when absent — the vast majority of the corpus (only highlight-born
    /// memories carry this).
    pub kb_author: Option<String>,
    /// CT-A1 — kb name of the artifact this memory was highlighted FROM,
    /// from `<meta name="kb-source-kb">`. None when absent.
    pub kb_source_kb: Option<String>,
    /// CT-A1 — artifact id (12-hex, path-derived) of that origin artifact,
    /// from `<meta name="kb-source-artifact">`. None when absent.
    pub kb_source_artifact: Option<String>,
    /// CT-A1 — the origin selection, from `<meta name="kb-source-anchor">`.
    /// Stored verbatim as the canonical `review::Anchor` JSON text (the same
    /// `lists::anchor_to_json` serialization `render_artifact` wrote) — this
    /// layer doesn't parse it into a structured `Anchor`, matching every
    /// other free-form meta here. None when absent.
    pub kb_source_anchor: Option<String>,

    /// L2 — memory visibility flag from `<meta name="kb-global">`.
    /// `true` when the content is the literal `"true"`. Used by the
    /// indexer's first-time seed pass into V0010 `memory_links` to add
    /// the `*` sentinel row. False (the default) means the memory is
    /// scoped to its explicit `kb_linked_kbs` set. Re-indexes after
    /// the seed never re-import this — V0011 tombstone wins.
    pub kb_global: bool,

    /// L2 — explicit kb-name set from `<meta name="kb-linked-kbs">`,
    /// comma-separated and slugified the same way as `kb-tags` (kb
    /// names are restricted to `[a-z0-9_-]+` by `KbName::new`, so the
    /// slugifier is conservative-safe). Used together with `kb_global`
    /// by the seed pass.
    pub kb_linked_kbs: Vec<String>,

    // --- FTS fields (topic 01 schema) ----------------------------------------
    pub headings: String,
    pub code: String,
    pub body: String,
    pub body_text_excerpt: String,

    // --- v0.6 I1 (gallery card glyph strip + summary) ------------------------
    /// Number of `<table>` elements.
    pub table_count: u32,
    /// Number of distinct `<pre>` code-block containers. We count outer
    /// `<pre>` blocks (semantic units) rather than every `pre`-or-
    /// `pre code` match — a `<pre><code>` pair would otherwise tally
    /// twice.
    pub code_block_count: u32,
    /// Word count of the parsed body text. Whitespace-delimited; cheap
    /// to compute since we already have the body string.
    pub word_count: u32,
    /// Convenience: `word_count > 1500`. The SPA capability glyph for
    /// "long-read" reads this; surfacing the precomputed flag saves
    /// the gallery a per-card branch.
    pub longread: bool,

    /// N-track — GFM task-list progress. `task_total` counts every
    /// `<input type="checkbox">` the render produced (comrak emits one per
    /// `- [ ]` / `- [x]` item, in document order); `task_done` counts those
    /// with the `checked` attribute. For notes (Markdown rendered via
    /// `markdown::render_fragment`) this feeds the SPA progress bar; ordinary
    /// HTML artifacts are 0/0 unless they hand-author checkboxes. The Nth
    /// checkbox here matches the Nth task line `notes::scan_tasks` finds —
    /// pinned by `task_counts_match_source_scan`.
    pub task_done: u32,
    pub task_total: u32,
}

/// Static / light / interactive / rich bucketing for inline JS or CSS bytes.
/// Topic 07 §D categorisation; thresholds are deliberately coarse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocBucket {
    #[default]
    Static,
    Light,
    Interactive,
    Rich,
}

impl LocBucket {
    /// Bucket from byte length of inline content.
    /// 0 → Static, 1-1023 → Light, 1024-10239 → Interactive, ≥10240 → Rich.
    pub fn from_bytes(bytes: usize) -> Self {
        match bytes {
            0 => LocBucket::Static,
            1..=1023 => LocBucket::Light,
            1024..=10239 => LocBucket::Interactive,
            _ => LocBucket::Rich,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LocBucket::Static => "static",
            LocBucket::Light => "light",
            LocBucket::Interactive => "interactive",
            LocBucket::Rich => "rich",
        }
    }
}

// --- Selector cache ----------------------------------------------------------

struct ParserSet {
    title: Selector,
    h1: Selector,
    p: Selector,
    svg: Selector,
    details: Selector,
    script: Selector,
    style: Selector,
    form: Selector,
    canvas: Selector,
    math: Selector,
    body: Selector,
    headings: Selector,
    code_blocks: Selector,
    /// I1 — distinct `<pre>` outer blocks (semantic code-block units).
    pre_outer: Selector,
    /// I1 — `<table>` count for the gallery glyph strip.
    table: Selector,
    /// N-track — GFM task-list checkboxes (`<input type="checkbox">`), for
    /// the notes progress count.
    task_checkbox: Selector,
    meta_category: Selector,
    /// T1 — `<meta name="kb-tags" content="rust, async, errors">`.
    meta_tags: Selector,
    /// S1 — `<meta name="kb-status" content="open">`.
    meta_status: Selector,
    /// S1 — `<meta name="kb-severity" content="medium">`.
    meta_severity: Selector,
    /// M1 — `<meta name="kb-salience" content="0.8">`.
    meta_salience: Selector,
    /// M1 — `<meta name="kb-decay" content="slow">`.
    meta_decay: Selector,
    /// M1 — `<meta name="kb-supersedes" content="7f3a1c…">`.
    meta_supersedes: Selector,
    /// S1 — `<meta name="kb-session" content="<claude-code-session-id>">`.
    meta_session: Selector,
    /// RA3 — `<meta name="kb-created" content="<unix-secs>">`.
    meta_created: Selector,
    /// RA4 — `<meta name="kb-summary" content="<one-line gloss>">`.
    meta_summary: Selector,
    /// MI-W3.3a — `<meta name="kb-memory-type" content="semantic">`.
    meta_memory_type: Selector,
    /// MI-W3.4 — `<meta name="kb-source" content="fetched-web">`.
    meta_trust_source: Selector,
    /// CT-A1 (U3) — `<meta name="kb-author" content="you">`.
    meta_author: Selector,
    /// CT-A1 — `<meta name="kb-source-kb" content="kb-docs">`.
    meta_source_kb: Selector,
    /// CT-A1 — `<meta name="kb-source-artifact" content="a1b2c3d4e5f6">`.
    meta_source_artifact: Selector,
    /// CT-A1 — `<meta name="kb-source-anchor" content="{…}">`.
    meta_source_anchor: Selector,
    /// L2 — `<meta name="kb-global" content="true">`.
    meta_global: Selector,
    /// L2 — `<meta name="kb-linked-kbs" content="kb-a, kb-b">`.
    meta_linked_kbs: Selector,
    template_prompt: Selector,
    draggable: Selector,
    animate_svg: Selector,
    anchors: Selector,
}

impl ParserSet {
    fn instance() -> &'static Self {
        static SET: OnceLock<ParserSet> = OnceLock::new();
        SET.get_or_init(|| ParserSet {
            title: Selector::parse("title").unwrap(),
            h1: Selector::parse("h1").unwrap(),
            p: Selector::parse("p").unwrap(),
            svg: Selector::parse("svg").unwrap(),
            details: Selector::parse("details").unwrap(),
            script: Selector::parse("script").unwrap(),
            style: Selector::parse("style").unwrap(),
            form: Selector::parse("form").unwrap(),
            canvas: Selector::parse("canvas").unwrap(),
            math: Selector::parse("math").unwrap(),
            body: Selector::parse("body").unwrap(),
            headings: Selector::parse("h1, h2, h3, h4, h5, h6, summary").unwrap(),
            code_blocks: Selector::parse("pre code, pre").unwrap(),
            pre_outer: Selector::parse("pre").unwrap(),
            table: Selector::parse("table").unwrap(),
            task_checkbox: Selector::parse(r#"input[type="checkbox"]"#).unwrap(),
            meta_category: Selector::parse(r#"meta[name="kb-category"]"#).unwrap(),
            meta_tags: Selector::parse(r#"meta[name="kb-tags"]"#).unwrap(),
            meta_status: Selector::parse(r#"meta[name="kb-status"]"#).unwrap(),
            meta_severity: Selector::parse(r#"meta[name="kb-severity"]"#).unwrap(),
            meta_salience: Selector::parse(r#"meta[name="kb-salience"]"#).unwrap(),
            meta_decay: Selector::parse(r#"meta[name="kb-decay"]"#).unwrap(),
            meta_supersedes: Selector::parse(r#"meta[name="kb-supersedes"]"#).unwrap(),
            meta_session: Selector::parse(r#"meta[name="kb-session"]"#).unwrap(),
            meta_created: Selector::parse(r#"meta[name="kb-created"]"#).unwrap(),
            meta_summary: Selector::parse(r#"meta[name="kb-summary"]"#).unwrap(),
            meta_memory_type: Selector::parse(r#"meta[name="kb-memory-type"]"#).unwrap(),
            meta_trust_source: Selector::parse(r#"meta[name="kb-source"]"#).unwrap(),
            meta_author: Selector::parse(r#"meta[name="kb-author"]"#).unwrap(),
            meta_source_kb: Selector::parse(r#"meta[name="kb-source-kb"]"#).unwrap(),
            meta_source_artifact: Selector::parse(r#"meta[name="kb-source-artifact"]"#).unwrap(),
            meta_source_anchor: Selector::parse(r#"meta[name="kb-source-anchor"]"#).unwrap(),
            meta_global: Selector::parse(r#"meta[name="kb-global"]"#).unwrap(),
            meta_linked_kbs: Selector::parse(r#"meta[name="kb-linked-kbs"]"#).unwrap(),
            template_prompt: Selector::parse(r#"template[id="kb-prompt"]"#).unwrap(),
            draggable: Selector::parse(r#"[draggable="true"]"#).unwrap(),
            animate_svg: Selector::parse("animate, animateTransform, animateMotion").unwrap(),
            anchors: Selector::parse("a[href]").unwrap(),
        })
    }
}

// --- Public API --------------------------------------------------------------

/// Extract every field defined by `Fields` from a parsed HTML document.
///
/// Selectors are cached globally; this function is cheap to call from a hot
/// indexer loop. Topic 07 budget: < 5 ms/file. Spike-walker baseline: ~160 µs.
pub fn extract(html: &str) -> Fields {
    let doc = Html::parse_document(html);
    let p = ParserSet::instance();

    let title = first_text(&doc, &p.title);
    let h1 = first_text(&doc, &p.h1);
    let first_p = first_text(&doc, &p.p);

    let svg_count = doc.select(&p.svg).count() as u32;
    let has_details = doc.select(&p.details).next().is_some();
    let has_form = doc.select(&p.form).next().is_some();
    let has_canvas = doc.select(&p.canvas).next().is_some();
    let has_math = doc.select(&p.math).next().is_some();
    let has_drag =
        doc.select(&p.draggable).next().is_some() || html_contains_inline_drag_handler(html);

    let kb_category = doc
        .select(&p.meta_category)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    let tags: Vec<String> = doc
        .select(&p.meta_tags)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|content| {
            content
                .split(',')
                .map(slugify_tag)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let kb_status = doc
        .select(&p.meta_status)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    let kb_severity = doc
        .select(&p.meta_severity)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    // M1 — salience parses to a clamped f32; unparseable / non-finite content
    // is None. `NaN`/`inf` must be rejected (NaN survives `clamp` and would
    // silently drop the memory from recall, since `score > floor` is false).
    let kb_salience = doc
        .select(&p.meta_salience)
        .next()
        .and_then(|el| el.value().attr("content"))
        .and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 1.0));

    let kb_decay = doc
        .select(&p.meta_decay)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    let kb_supersedes = doc
        .select(&p.meta_supersedes)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    let kb_session = doc
        .select(&p.meta_session)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    // RA3 — write-time decay basis. Unparseable / non-numeric content → None.
    let kb_created = doc
        .select(&p.meta_created)
        .next()
        .and_then(|el| el.value().attr("content"))
        .and_then(|s| s.trim().parse::<i64>().ok());

    // RA4 — one-line memory summary.
    let kb_summary = doc
        .select(&p.meta_summary)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    // MI-W3.3a — optional CoALA-minimal type. Free-form at this layer
    // (validated at the write boundary); a lowercase-normalized trim like
    // every other memory meta.
    let kb_memory_type = doc
        .select(&p.meta_memory_type)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    // MI-W3.4 — write-time trust tag. Same free-form-at-this-layer pattern.
    let kb_source = doc
        .select(&p.meta_trust_source)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    // CT-A1 (U3 parse-back) — highlight provenance. Same free-form-string
    // pattern as every other meta above; `kb_source_anchor` is carried as
    // the raw `lists::anchor_to_json` text, not parsed into an `Anchor`.
    let kb_author = doc
        .select(&p.meta_author)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    let kb_source_kb = doc
        .select(&p.meta_source_kb)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    let kb_source_artifact = doc
        .select(&p.meta_source_artifact)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    let kb_source_anchor = doc
        .select(&p.meta_source_anchor)
        .next()
        .and_then(|el| el.value().attr("content").map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty());

    // L2 — visibility flag is "true" iff the content is the literal
    // "true" (case-insensitive after trim). Any other value (incl.
    // "false", "0", missing) parses to false.
    let kb_global = doc
        .select(&p.meta_global)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|s| s.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // L2 — comma-separated kb names. Mirrors the kb-tags pattern: split
    // on comma, slugify (kb names are already [a-z0-9_-]+ so the
    // slugifier is conservative-safe), drop empty fragments.
    let kb_linked_kbs: Vec<String> = doc
        .select(&p.meta_linked_kbs)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|content| {
            content
                .split(',')
                .map(slugify_tag)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let prompt = doc.select(&p.template_prompt).next().map(|el| {
        let inner = el.inner_html();
        let trimmed = inner.trim();
        if trimmed.len() > PROMPT_MAX_BYTES {
            // v0.7.1 P2 — truncate on a *byte* boundary, not a char
            // count. `chars().take(PROMPT_MAX_BYTES)` kept up to that
            // many Unicode scalars — ~4x the cap for CJK / emoji — so a
            // non-ASCII prompt blew past the documented 8 KB cap.
            let mut cut = PROMPT_MAX_BYTES;
            while cut > 0 && !trimmed.is_char_boundary(cut) {
                cut -= 1;
            }
            trimmed[..cut].to_string()
        } else {
            trimmed.to_string()
        }
    });
    let prompt_size_bytes = prompt.as_ref().map(|s| s.len() as u32).unwrap_or(0);

    let css_total: usize = doc
        .select(&p.style)
        .map(|el| el.text().map(str::len).sum::<usize>())
        .sum();
    // Single pass over <script> elements computes the existence flag and
    // the byte total together — previously the tree was traversed twice
    // (once for `has_script`, once for `js_total`).
    let mut has_script = false;
    let mut js_total = 0usize;
    for el in doc.select(&p.script) {
        has_script = true;
        js_total += el.text().map(str::len).sum::<usize>();
    }
    let css_loc = LocBucket::from_bytes(css_total);
    let js_loc = LocBucket::from_bytes(js_total);

    let has_animation = css_total > 0 && contains_keyframes(&doc, &p.style)
        || doc.select(&p.animate_svg).next().is_some();

    let headings = collect_text_lines(&doc, &p.headings);
    let code = collect_text_blocks(&doc, &p.code_blocks);
    let body = body_text(&doc, &p.body);
    let body_text_excerpt = body
        .chars()
        .take(BODY_EXCERPT_MAX_CHARS)
        .collect::<String>();

    // I1 — counters for the gallery card glyph strip.
    let table_count = doc.select(&p.table).count() as u32;
    let code_block_count = doc.select(&p.pre_outer).count() as u32;
    let word_count = body.split_whitespace().count() as u32;
    let longread = word_count > 1500;

    // N-track — GFM task-list progress. comrak renders a done item as
    // `<input type="checkbox" checked disabled>` and a todo as
    // `<input type="checkbox" disabled>`, in document order — so counting
    // checkboxes here matches the Nth task line `notes::scan_tasks` finds.
    let mut task_total = 0u32;
    let mut task_done = 0u32;
    for el in doc.select(&p.task_checkbox) {
        task_total += 1;
        if el.value().attr("checked").is_some() {
            task_done += 1;
        }
    }

    Fields {
        title,
        h1,
        p: first_p,
        svg_count,
        has_details,
        has_script,
        has_form,
        has_canvas,
        has_animation,
        has_math,
        has_drag,
        kb_category,
        prompt,
        prompt_size_bytes,
        js_loc,
        css_loc,
        tags,
        kb_status,
        kb_severity,
        kb_salience,
        kb_decay,
        kb_supersedes,
        kb_session,
        kb_created,
        kb_summary,
        kb_memory_type,
        kb_source,
        kb_author,
        kb_source_kb,
        kb_source_artifact,
        kb_source_anchor,
        kb_global,
        kb_linked_kbs,
        headings,
        code,
        body,
        body_text_excerpt,
        table_count,
        code_block_count,
        word_count,
        longread,
        task_done,
        task_total,
    }
}

/// Generic directory names that don't carry signal — used by
/// `path_derived_tags` to drop noise. Mirrors the SPA's
/// `web/src/lib/derive.ts` GENERIC_DIRS list so the same name → tag
/// rule applies whether the daemon or the SPA derives.
const GENERIC_DIRS: &[&str] = &[
    "artifacts",
    "kb",
    "html",
    "public",
    "docs",
    "src",
    "src-tauri",
    "node_modules",
    "dist",
    "build",
];

/// v0.6 T1 — derive tags from a filesystem path when the artifact
/// has no `<meta name="kb-tags">`. Takes the last two non-generic
/// directory segments before the file name, slugifies them. Used by
/// the indexer when `parser::Fields.tags` is empty.
///
/// Returns at most two tags. Order: more-specific first (the segment
/// closer to the file). Generic-named segments are filtered out.
pub fn path_derived_tags(path: &str) -> Vec<String> {
    // LOW (deep-review): split on both `/` and `\\` so Windows-shaped
    // paths (`foo\bar\baz.html`) tokenize the same as unix. kb is
    // Linux-only today, but the cost of accepting both separators
    // is one extra char in the split set.
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    if parts.len() <= 1 {
        return Vec::new();
    }
    let dirs = &parts[..parts.len() - 1];
    let mut out: Vec<String> = Vec::new();
    for d in dirs.iter().rev() {
        if out.len() >= 2 {
            break;
        }
        let slug = slugify_tag(d);
        if slug.is_empty() || GENERIC_DIRS.contains(&slug.as_str()) {
            continue;
        }
        if !out.contains(&slug) {
            out.push(slug);
        }
    }
    out
}

/// Normalise a free-text tag to a slug: lowercase ASCII alphanumerics,
/// runs of any other char collapse to a single `-`, leading/trailing
/// dashes trimmed. The single source of truth for tag slugging — the
/// indexer applies it at parse time and the `PATCH …/meta` route applies
/// the identical transform before writing tags back, so a round-trip is a
/// fixed point.
pub fn slugify_tag(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Extract [`Fields`] from a Markdown source. Renders the body to an HTML
/// fragment (the SAME `markdown::render_fragment` the serve path wraps),
/// wraps it, and runs the HTML [`extract`] for body/headings/code/counts/
/// capabilities + any inline `<template id="kb-prompt">`. The flat
/// frontmatter then overrides the metadata facets (it is the markdown
/// author's source of truth). Returns the Fields plus the wrapped HTML so
/// the indexer can derive cross-artifact link edges against the same
/// rendered DOM the serve path produces. Tags + linked-kbs are slugified
/// identically to the HTML `<meta>` path.
pub fn extract_markdown(src: &str) -> (Fields, String) {
    let (fm, body) = crate::markdown::parse_frontmatter(src);
    let wrapped = format!(
        "<!doctype html><html><body>\n{}\n</body></html>",
        crate::markdown::render_fragment(body)
    );
    let mut f = extract(&wrapped);
    // Title: frontmatter wins, else the body's first `# H1`, else whatever H1
    // the rendered fragment produced.
    f.title = fm
        .title
        .clone()
        .or_else(|| crate::markdown::first_h1(body))
        .or_else(|| f.h1.clone());
    if fm.kb_category.is_some() {
        f.kb_category = fm.kb_category;
    }
    if !fm.kb_tags.is_empty() {
        f.tags = fm
            .kb_tags
            .iter()
            .map(|t| slugify_tag(t))
            .filter(|s| !s.is_empty())
            .collect();
    }
    if fm.kb_status.is_some() {
        f.kb_status = fm.kb_status;
    }
    if fm.kb_severity.is_some() {
        f.kb_severity = fm.kb_severity;
    }
    if fm.kb_salience.is_some() {
        f.kb_salience = fm.kb_salience;
    }
    if fm.kb_decay.is_some() {
        f.kb_decay = fm.kb_decay;
    }
    if fm.kb_supersedes.is_some() {
        f.kb_supersedes = fm.kb_supersedes;
    }
    if fm.kb_session.is_some() {
        f.kb_session = fm.kb_session;
    }
    if fm.kb_global {
        f.kb_global = true;
    }
    if !fm.kb_linked_kbs.is_empty() {
        f.kb_linked_kbs = fm
            .kb_linked_kbs
            .iter()
            .map(|t| slugify_tag(t))
            .filter(|s| !s.is_empty())
            .collect();
    }
    (f, wrapped)
}

/// v0.3 — extract every cross-artifact link from the document. Two
/// shapes are recognised:
///
/// 1. `<a href="http://<id><artifact_host_suffix>[:port]/...">` —
///    the iframe-sandboxed canonical URL the daemon serves. The
///    suffix comes from `[server] artifact_host_suffix` (default
///    `.artifacts.localhost`).
/// 2. `<a href="/a/<kb>/<path-or-id>">` — the SPA permalink, carrying
///    either the artifact's source-relative path (e.g.
///    `pm/01-timeline.html`) or its bare 12-hex id. The kb name is
///    discarded (the indexer scopes the edge to the host kb anyway);
///    a path resolves to its id deterministically via
///    `ArtifactId::from_path` — same full-path semantics as
///    `share::classify_cross_link`.
///
/// Returns deduplicated artifact ids in document order. Hrefs that
/// don't match either shape (external links, mailto:, in-document
/// `#frag`, etc.) are dropped silently. Used by the indexer to
/// populate the sqlite `edges` table after every successful upsert;
/// `routes::graph` reads it back for the cross-artifact view.
pub fn extract_links(html: &str, artifact_host_suffix: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let p = ParserSet::instance();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for el in doc.select(&p.anchors) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        if let Some(id) = parse_artifact_link(href, artifact_host_suffix) {
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
    }
    out
}

/// Extract every `<a href>` value from the document, in document order,
/// filtered to hrefs that *could* point at another file under the kb's
/// source tree:
///
/// - drops empty, in-doc `#frag`, `mailto:`, `tel:`, `javascript:` hrefs;
/// - drops absolute URLs (`http://`, `https://`, `//host`) — those are
///   handled by [`extract_links`] when they target an artifact subdomain;
/// - keeps everything else: root-anchored paths (`/foo/bar.html`),
///   sibling-relative (`./sibling.html`, `sibling.html`), and parent-
///   traversing relative paths (`../foo.html`).
///
/// The indexer resolves the kept hrefs against the source file's parent
/// directory and looks them up by absolute path, populating cross-
/// artifact edges that the SPA permalink / subdomain shapes miss.
pub fn extract_link_hrefs(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let p = ParserSet::instance();
    let mut out = Vec::new();
    for el in doc.select(&p.anchors) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        let trimmed = href.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("mailto:")
            || lower.starts_with("tel:")
            || lower.starts_with("javascript:")
            || lower.starts_with("http://")
            || lower.starts_with("https://")
            || trimmed.starts_with("//")
        {
            continue;
        }
        out.push(trimmed.to_string());
    }
    out
}

/// Single-parse variant returning both [`extract_links`] (artifact ids,
/// deduped, document order) and [`extract_link_hrefs`] (relative-path
/// hrefs, document order, duplicates kept) from a single
/// `Html::parse_document` and anchor walk. The indexer uses this so a
/// reindex parses each document's anchors once instead of twice; the
/// single-purpose variants remain for callers/tests that need only one
/// side. The two output Vecs are byte-identical to calling the pair
/// separately.
pub fn extract_links_and_hrefs(
    html: &str,
    artifact_host_suffix: &str,
) -> (Vec<String>, Vec<String>) {
    let doc = Html::parse_document(html);
    let p = ParserSet::instance();
    let mut seen = std::collections::HashSet::new();
    let mut links = Vec::new();
    let mut hrefs = Vec::new();
    for el in doc.select(&p.anchors) {
        let Some(href) = el.value().attr("href") else {
            continue;
        };
        // Artifact-id links (subdomain / SPA permalink), deduped — mirrors
        // `extract_links`.
        if let Some(id) = parse_artifact_link(href, artifact_host_suffix) {
            if seen.insert(id.clone()) {
                links.push(id);
            }
        }
        // Relative-path href candidates (order preserved, dupes kept) —
        // mirrors `extract_link_hrefs`.
        let trimmed = href.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("mailto:")
            || lower.starts_with("tel:")
            || lower.starts_with("javascript:")
            || lower.starts_with("http://")
            || lower.starts_with("https://")
            || trimmed.starts_with("//")
        {
            continue;
        }
        hrefs.push(trimmed.to_string());
    }
    (links, hrefs)
}

/// Map an `<a href>` value to an artifact id, or `None` if the link
/// doesn't reference an artifact in this kb.
///
/// The SPA-permalink branch accepts BOTH permalink forms:
/// - `/a/{kb}/{12-hex-id}` — the id is taken verbatim;
/// - `/a/{kb}/{source-relative-path}` (the shape
///   docs/authoring-artifacts.md tells authors to write, e.g.
///   `/a/mykb/pm/01-timeline.html`) — the FULL path after the kb
///   segment (percent-decoded) maps to its id deterministically via
///   `ArtifactId::from_path`, because ids ARE `sha256(rel_path)[:12]`
///   (root invariant #27). No storage lookup is needed; a dangling
///   path simply hashes to an id no artifact carries, and the edge
///   stays invisible — same as a dangling id-form link.
///
/// Before 2026-07-03 this took only the FIRST path segment verbatim
/// (`nth(1)`), so every documented path-form link produced a dead edge
/// key ("pm" for the example above) and backlinks never surfaced —
/// found live by the showcase corpus; `share::classify_cross_link` had
/// already implemented the full-path semantics for export rewriting.
fn parse_artifact_link(href: &str, artifact_host_suffix: &str) -> Option<String> {
    let trimmed = href.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    // SPA permalink: /a/{kb}/{id-or-source-rel-path}[?...][#...]
    if let Some(rest) = trimmed.strip_prefix("/a/") {
        let slash = rest.find('/')?; // need both a kb segment and a target
        let raw = rest[slash + 1..].split(['?', '#']).next()?;
        let decoded = crate::strutil::percent_decode(raw);
        let path = decoded.trim_end_matches('/');
        if path.is_empty() {
            return None;
        }
        // Reject traversal shapes and hidden-file segments — neither can
        // name an indexed artifact (dotfiles are skipped by the walker).
        if path
            .split('/')
            .any(|seg| seg.is_empty() || seg.starts_with('.'))
        {
            return None;
        }
        // id-form permalink: a single 12-lowercase-hex segment IS the id.
        if !path.contains('/')
            && path.len() == 12
            && path
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Some(path.to_string());
        }
        // path-form permalink: derive the id from the source-relative path.
        return Some(crate::ids::ArtifactId::from_path(path).as_str().to_string());
    }
    // Subdomain URL: http(s)?://<id><artifact_host_suffix>[:port]/...
    let after_scheme = if let Some(rest) = trimmed.strip_prefix("https://") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        rest
    } else {
        trimmed.strip_prefix("//")?
    };
    let host = after_scheme.split(['/', '?', '#']).next()?;
    crate::iframe::parse_artifact_id(host, artifact_host_suffix).map(|s| s.to_string())
}

/// Extract the document's headings (h1..h6) in document order. Each
/// returned tuple is `(level, text)` with text trimmed and empty
/// headings skipped. Level 1 = h1, ..., 6 = h6.
///
/// Used by the `/api/kb/{kb}/graph/{id}` route (TUI DETAIL prereq) to
/// derive a hierarchical outline of the artifact. Reuses the cached
/// selectors so it stays cheap even on large documents.
pub fn headings_in_order(html: &str) -> Vec<(u8, String)> {
    headings_in_order_doc(&Html::parse_document(html))
}

/// [`headings_in_order`] over an already-parsed document — lets the anchor
/// re-resolution loops (review/lists) share ONE parse across many anchors
/// instead of re-parsing the same HTML per Chapter anchor.
pub fn headings_in_order_doc(doc: &Html) -> Vec<(u8, String)> {
    let sel = Selector::parse("h1, h2, h3, h4, h5, h6").expect("static selector");
    let mut out = Vec::new();
    for el in doc.select(&sel) {
        let level: u8 = match el.value().name() {
            "h1" => 1,
            "h2" => 2,
            "h3" => 3,
            "h4" => 4,
            "h5" => 5,
            "h6" => 6,
            _ => continue,
        };
        let text = el.text().collect::<Vec<_>>().join(" ").trim().to_string();
        if !text.is_empty() {
            out.push((level, text));
        }
    }
    out
}

/// Block-structured plain text for diffing. Walks the document body and
/// emits one line per block-level element (`<p>`, `<h1>`…`<h6>`, `<li>`,
/// `<pre>`, table rows, …), with inline runs collapsed to single spaces and
/// `<script>`/`<style>`/`<template>`/`<noscript>` subtrees skipped.
///
/// Unlike [`Fields::body`] (every text node joined with single spaces — one
/// giant line), preserving block boundaries lets a line-level diff localise a
/// prose change to the paragraph that changed instead of reflowing the whole
/// document. This is the "focus on the text, not the HTML" input for the
/// Track V version diff; the raw-HTML diff uses the file bytes verbatim.
pub fn text_blocks(html: &str) -> String {
    let doc = Html::parse_document(html);
    let p = ParserSet::instance();
    let Some(body) = doc.select(&p.body).next() else {
        return String::new();
    };
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    // `*body` derefs the ElementRef to the underlying NodeRef.
    walk_blocks(*body, &mut lines, &mut cur);
    flush_line(&mut lines, &mut cur);
    lines.join("\n")
}

// --- Helpers -----------------------------------------------------------------

fn first_text(doc: &Html, sel: &Selector) -> Option<String> {
    doc.select(sel)
        .next()
        .map(|n| {
            n.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty())
}

fn collect_text_lines(doc: &Html, sel: &Selector) -> String {
    doc.select(sel)
        .map(|n| {
            n.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect_text_blocks(doc: &Html, sel: &Selector) -> String {
    doc.select(sel)
        .map(|n| n.text().collect::<String>())
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Concatenate all visible text under `<body>`, skipping descendants whose
/// nearest ancestor element is `<script>`, `<style>`, `<template>`, or
/// `<noscript>`. The spike-walker finding: SVG-heavy artifacts have no
/// top-level `<p>`, so a fallback excerpt over the body's visible text is
/// the right "first paragraph" proxy.
fn body_text(doc: &Html, body_sel: &Selector) -> String {
    let Some(body) = doc.select(body_sel).next() else {
        return String::new();
    };

    const SKIP_TAGS: &[&str] = &["script", "style", "template", "noscript"];

    let mut out = String::new();
    for descendant in body.descendants() {
        if let Some(text) = descendant.value().as_text() {
            if has_skip_ancestor(descendant, SKIP_TAGS) {
                continue;
            }
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(trimmed);
        }
    }
    out
}

/// True when any ancestor element of `node` is named in `skip_tags`.
///
/// `pub(crate)` for [`crate::coderefs`] (DCB W1.A), which must skip the same
/// inert subtrees this module does — sharing the walker beats re-implementing
/// it, since a divergence would let the `<template id="kb-prompt">` bundle
/// (invariant #5) leak into one consumer and not the other.
pub(crate) fn has_skip_ancestor(
    node: ego_tree::NodeRef<scraper::Node>,
    skip_tags: &[&str],
) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if let Some(el) = p.value().as_element() {
            if skip_tags.contains(&el.name()) {
                return true;
            }
        }
        cur = p.parent();
    }
    false
}

/// Block-level element names that force a line break in [`text_blocks`].
/// Container blocks (`div`/`section`/…) are included too — over-segmenting a
/// wrapper is harmless (empty lines are dropped), and it keeps the output
/// stable across markup that merely re-nests the same prose.
pub(crate) const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

/// Elements whose text content is not visible prose — skipped wholesale by
/// [`text_blocks`] (mirrors `body_text`'s `SKIP_TAGS`).
pub(crate) const TEXT_SKIP_TAGS: &[&str] = &["script", "style", "template", "noscript"];

/// Depth-first walk for [`text_blocks`]: append inline text to `cur`, and
/// flush `cur` to `lines` at every block boundary (and `<br>`).
fn walk_blocks(node: ego_tree::NodeRef<scraper::Node>, lines: &mut Vec<String>, cur: &mut String) {
    for child in node.children() {
        match child.value() {
            scraper::Node::Text(t) => {
                let piece = t.split_whitespace().collect::<Vec<_>>().join(" ");
                if !piece.is_empty() {
                    if !cur.is_empty() {
                        cur.push(' ');
                    }
                    cur.push_str(&piece);
                }
            }
            scraper::Node::Element(el) => {
                let name = el.name();
                if TEXT_SKIP_TAGS.contains(&name) {
                    continue;
                }
                if name == "br" {
                    flush_line(lines, cur);
                    continue;
                }
                if BLOCK_TAGS.contains(&name) {
                    flush_line(lines, cur);
                    walk_blocks(child, lines, cur);
                    flush_line(lines, cur);
                } else {
                    // Inline element (a, span, em, strong, code, …) — stay on
                    // the current line.
                    walk_blocks(child, lines, cur);
                }
            }
            _ => {}
        }
    }
}

/// Trim + push the current line buffer to `lines` (dropping it if blank),
/// then clear it.
fn flush_line(lines: &mut Vec<String>, cur: &mut String) {
    let line = cur.trim();
    if !line.is_empty() {
        lines.push(line.to_string());
    }
    cur.clear();
}

fn contains_keyframes(doc: &Html, style_sel: &Selector) -> bool {
    doc.select(style_sel).any(|el| {
        let text: String = el.text().collect();
        text.contains("@keyframes") || text.contains("@-webkit-keyframes")
    })
}

/// Lightweight inline-handler check: scan the raw HTML for `ondragstart=` or
/// `ondrag=` attributes. Cheap; doesn't require re-walking the parsed tree.
fn html_contains_inline_drag_handler(html: &str) -> bool {
    html.contains("ondragstart=") || html.contains("ondrag=")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- spike-walker baseline tests (all preserved) -------------------------

    #[test]
    fn empty_document_yields_default_fields() {
        let f = extract("");
        assert_eq!(f.title, None);
        assert_eq!(f.h1, None);
        assert_eq!(f.svg_count, 0);
        assert!(!f.has_script);
        assert!(!f.has_details);
    }

    #[test]
    fn minimal_document_extracts_title() {
        let f = extract("<html><head><title>hi</title></head><body></body></html>");
        assert_eq!(f.title.as_deref(), Some("hi"));
        assert!(f.h1.is_none());
        assert_eq!(f.svg_count, 0);
        assert!(!f.has_script);
    }

    #[test]
    fn whitespace_in_title_is_collapsed() {
        let f = extract("<title>  hi\n  there\t</title>");
        assert_eq!(f.title.as_deref(), Some("hi there"));
    }

    #[test]
    fn first_h1_and_p_only() {
        let html = "<h1>first</h1><h1>second</h1><p>p1</p><p>p2</p>";
        let f = extract(html);
        assert_eq!(f.h1.as_deref(), Some("first"));
        assert_eq!(f.p.as_deref(), Some("p1"));
    }

    #[test]
    fn counts_svgs_and_flags_features() {
        let html = "<svg></svg><svg></svg><svg></svg><details></details><script></script>";
        let f = extract(html);
        assert_eq!(f.svg_count, 3);
        assert!(f.has_details);
        assert!(f.has_script);
    }

    #[test]
    fn counts_tables_and_pre_blocks() {
        let html = "<table></table><table></table><pre>a</pre><pre><code>b</code></pre>";
        let f = extract(html);
        assert_eq!(f.table_count, 2);
        // `<pre>` counted once even with inner `<code>` — semantic
        // code-block units, not raw selector matches.
        assert_eq!(f.code_block_count, 2);
    }

    #[test]
    fn word_count_and_longread_flag() {
        let short = format!("<body><p>{}</p></body>", "word ".repeat(50));
        let s = extract(&short);
        assert_eq!(s.word_count, 50);
        assert!(!s.longread);

        let long = format!("<body><p>{}</p></body>", "word ".repeat(1501));
        let l = extract(&long);
        assert_eq!(l.word_count, 1501);
        assert!(l.longread);
    }

    #[test]
    fn html_without_tasks_counts_zero() {
        let f = extract("<ul><li>a</li><li>b</li></ul>");
        assert_eq!(f.task_total, 0);
        assert_eq!(f.task_done, 0);
    }

    #[test]
    fn task_counts_from_rendered_markdown() {
        // comrak emits one `<input type=checkbox>` per GFM task item, with
        // `checked` on done items.
        let html = crate::markdown::render_fragment("- [x] done\n- [ ] todo\n- [X] also done\n");
        let wrapped = format!("<body>{html}</body>");
        let f = extract(&wrapped);
        assert_eq!(f.task_total, 3);
        assert_eq!(f.task_done, 2);
    }

    // invariant:16 task-ordinal
    #[test]
    fn task_counts_match_source_scan() {
        // The load-bearing N-track invariant: the parser's rendered-HTML
        // count (which feeds the lance columns + progress bar) MUST equal
        // `notes::checklist_counts` over the source body (which drives the
        // toggle-by-index). If comrak's task detection ever diverged from
        // the source scanner, this fails loudly.
        for body in [
            "- [ ] a\n- [x] b\n",
            "- [x] a\n  - [ ] nested\n- [ ] c\n",
            "intro\n\n- [ ] x\n- [ ] y\n- [x] z\n\nouttro\n",
            "* [ ] star marker\n+ [x] plus marker\n",
            "- [ ] real\n```\n- [ ] fenced fake\n```\n- [x] real2\n",
            // N-track divergent cases — the hand-rolled scanner missed these,
            // so comrak (count) and the scanner (toggle index) disagreed.
            // The AST scanner makes them agree by construction; this pins it.
            "> - [ ] blockquoted\n> - [x] blockquoted2\n", // blockquote
            "1. [ ] one\n2. [x] two\n",                    // ordered (dot)
            "1) [ ] one\n2) [x] two\n",                    // ordered (paren)
            "- [ ]x no space after bracket\n",             // GFM rejects → 0
            "````\n```\n- [ ] still fenced\n````\n- [x] real\n", // 4-tick over 3
            "~~~~\n- [ ] fenced tilde\n~~~~\n- [x] real\n", // tilde fence
            "- [ ] a\n1. [x] ordered\n> - [ ] bq\n",       // mixed forms
            "# 日本語\n\n- [ ] 最初\n- [x] second\n",      // multibyte prior line
        ] {
            let html = crate::markdown::render_fragment(body);
            let wrapped = format!("<body>{html}</body>");
            let f = extract(&wrapped);
            let (done, total) = crate::notes::checklist_counts(body);
            assert_eq!(
                (f.task_done, f.task_total),
                (done, total),
                "rendered count must match source scan for body: {body:?}"
            );
        }
    }

    #[test]
    fn malformed_inputs_do_not_panic() {
        for input in [
            "",
            "<html><body><p>unclosed",
            "<!--><html",
            "<html><body><script>alert('<title>x</title>')</script></body></html>",
            "<svg><svg><svg></svg></svg></svg>",
            "<><></><<><><><><><><>",
        ] {
            let _ = extract(input);
        }
    }

    // --- 9-facet extension tests ---------------------------------------------

    #[test]
    fn detects_form_canvas_math() {
        let html = "<form><input/></form><canvas></canvas><math><mi>x</mi></math>";
        let f = extract(html);
        assert!(f.has_form);
        assert!(f.has_canvas);
        assert!(f.has_math);
    }

    #[test]
    fn detects_drag_via_attribute() {
        let html = r#"<ul><li draggable="true">item</li></ul>"#;
        let f = extract(html);
        assert!(f.has_drag);
    }

    #[test]
    fn detects_drag_via_inline_handler() {
        let html = r#"<div ondragstart="x()">drag me</div>"#;
        let f = extract(html);
        assert!(f.has_drag);
    }

    #[test]
    fn detects_animation_via_keyframes() {
        let html = "<style>@keyframes pulse { 0% { opacity: 0; } 100% { opacity: 1; } }</style>";
        let f = extract(html);
        assert!(f.has_animation);
    }

    #[test]
    fn detects_animation_via_svg_animate() {
        let html = "<svg><animate attributeName='x' /></svg>";
        let f = extract(html);
        assert!(f.has_animation);
    }

    #[test]
    fn no_animation_for_plain_styles() {
        let html = "<style>body { color: red; }</style>";
        let f = extract(html);
        assert!(!f.has_animation);
    }

    #[test]
    fn extracts_kb_category_from_meta() {
        let html = r#"<meta name="kb-category" content="Code-Review"/>"#;
        let f = extract(html);
        assert_eq!(f.kb_category.as_deref(), Some("Code-Review"));
    }

    #[test]
    fn extracts_kb_status_from_meta() {
        let html = r#"<meta name="kb-status" content="open"/>"#;
        let f = extract(html);
        assert_eq!(f.kb_status.as_deref(), Some("open"));
    }

    #[test]
    fn extracts_kb_severity_from_meta() {
        let html = r#"<meta name="kb-severity" content="medium"/>"#;
        let f = extract(html);
        assert_eq!(f.kb_severity.as_deref(), Some("medium"));
    }

    #[test]
    fn missing_kb_status_and_severity_yield_none() {
        let f = extract("<title>doc</title>");
        assert!(f.kb_status.is_none());
        assert!(f.kb_severity.is_none());
    }

    #[test]
    fn empty_kb_status_content_yields_none() {
        let html = r#"<meta name="kb-status" content="   "/>"#;
        let f = extract(html);
        assert!(f.kb_status.is_none());
    }

    #[test]
    fn kb_status_and_severity_preserve_case_unlike_tags() {
        // Unlike tags (which slugify to lowercase-hyphenated), status
        // and severity are surfaced verbatim — vocabularies vary per
        // deployment (e.g. "sev-1" vs "SEV1" vs "Critical").
        let html = r#"
            <meta name="kb-status" content="In Progress"/>
            <meta name="kb-severity" content="SEV-2"/>
        "#;
        let f = extract(html);
        assert_eq!(f.kb_status.as_deref(), Some("In Progress"));
        assert_eq!(f.kb_severity.as_deref(), Some("SEV-2"));
    }

    #[test]
    fn extracts_kb_salience_from_meta() {
        let f = extract(r#"<meta name="kb-salience" content="0.5"/>"#);
        assert_eq!(f.kb_salience, Some(0.5));
    }

    #[test]
    fn kb_salience_clamps_to_unit_range() {
        assert_eq!(
            extract(r#"<meta name="kb-salience" content="1.7"/>"#).kb_salience,
            Some(1.0)
        );
        assert_eq!(
            extract(r#"<meta name="kb-salience" content="-0.3"/>"#).kb_salience,
            Some(0.0)
        );
    }

    #[test]
    fn unparseable_or_absent_kb_salience_yields_none() {
        assert!(extract(r#"<meta name="kb-salience" content="high"/>"#)
            .kb_salience
            .is_none());
        assert!(extract("<title>no salience</title>").kb_salience.is_none());
    }

    #[test]
    fn extracts_kb_decay_and_supersedes_from_meta() {
        let html = r#"
            <meta name="kb-decay" content="fast"/>
            <meta name="kb-supersedes" content="7f3a1c0d2e4b"/>
        "#;
        let f = extract(html);
        assert_eq!(f.kb_decay.as_deref(), Some("fast"));
        assert_eq!(f.kb_supersedes.as_deref(), Some("7f3a1c0d2e4b"));
    }

    #[test]
    fn extracts_kb_session_from_meta() {
        let html = r#"<meta name="kb-session" content="abc-1234-session-id"/>"#;
        let f = extract(html);
        assert_eq!(f.kb_session.as_deref(), Some("abc-1234-session-id"));
    }

    // invariant:10 kb-created
    #[test]
    fn extracts_kb_created_from_meta() {
        // RA3 — numeric content parses; absent/garbage → None.
        assert_eq!(
            extract(r#"<meta name="kb-created" content="1700000000"/>"#).kb_created,
            Some(1_700_000_000)
        );
        assert!(extract("<title>no created</title>").kb_created.is_none());
        assert!(
            extract(r#"<meta name="kb-created" content="not-a-number"/>"#)
                .kb_created
                .is_none()
        );
    }

    #[test]
    fn empty_or_absent_kb_session_yields_none() {
        assert!(extract("<title>no session</title>").kb_session.is_none());
        assert!(extract(r#"<meta name="kb-session" content="   "/>"#)
            .kb_session
            .is_none());
    }

    // CT-A1 (U3 parse-back)
    #[test]
    fn extracts_u3_provenance_from_meta() {
        let html = r#"
            <meta name="kb-author" content="you"/>
            <meta name="kb-source-kb" content="kb-docs"/>
            <meta name="kb-source-artifact" content="a1b2c3d4e5f6"/>
            <meta name="kb-source-anchor" content="{&quot;kind&quot;:&quot;section&quot;,&quot;id&quot;:&quot;intro&quot;}"/>
        "#;
        let f = extract(html);
        assert_eq!(f.kb_author.as_deref(), Some("you"));
        assert_eq!(f.kb_source_kb.as_deref(), Some("kb-docs"));
        assert_eq!(f.kb_source_artifact.as_deref(), Some("a1b2c3d4e5f6"));
        assert_eq!(
            f.kb_source_anchor.as_deref(),
            Some(r#"{"kind":"section","id":"intro"}"#)
        );
    }

    #[test]
    fn empty_or_absent_u3_provenance_yields_none() {
        let f = extract("<title>no provenance</title>");
        assert!(f.kb_author.is_none());
        assert!(f.kb_source_kb.is_none());
        assert!(f.kb_source_artifact.is_none());
        assert!(f.kb_source_anchor.is_none());
        assert!(extract(r#"<meta name="kb-author" content="   "/>"#)
            .kb_author
            .is_none());
    }

    // U3's `kb-source-kb`/`kb-source-artifact`/`kb-source-anchor` metas must
    // never collide with MI-W3.4's UNRELATED `kb-source` trust tag — the CSS
    // attribute selector is an exact match, not a prefix, but this pins the
    // distinction so a future rename can't quietly merge them.
    #[test]
    fn u3_provenance_metas_are_distinct_from_the_kb_source_trust_tag() {
        let html = r#"
            <meta name="kb-source" content="fetched-web"/>
            <meta name="kb-source-kb" content="kb-docs"/>
        "#;
        let f = extract(html);
        assert_eq!(f.kb_source.as_deref(), Some("fetched-web"));
        assert_eq!(f.kb_source_kb.as_deref(), Some("kb-docs"));
    }

    #[test]
    fn extracts_kb_global_true_only_for_literal_true() {
        assert!(extract(r#"<meta name="kb-global" content="true"/>"#).kb_global);
        assert!(extract(r#"<meta name="kb-global" content="TRUE"/>"#).kb_global);
        assert!(extract(r#"<meta name="kb-global" content="  true  "/>"#).kb_global);
        assert!(!extract(r#"<meta name="kb-global" content="false"/>"#).kb_global);
        assert!(!extract(r#"<meta name="kb-global" content="1"/>"#).kb_global);
        assert!(!extract("<title>no flag</title>").kb_global);
    }

    #[test]
    fn extracts_kb_linked_kbs_csv() {
        let f = extract(r#"<meta name="kb-linked-kbs" content="kb-a, kb-b , KB-C"/>"#);
        assert_eq!(f.kb_linked_kbs, vec!["kb-a", "kb-b", "kb-c"]);
    }

    #[test]
    fn empty_kb_linked_kbs_yields_empty_vec() {
        assert!(extract("<title>no links</title>").kb_linked_kbs.is_empty());
        assert!(extract(r#"<meta name="kb-linked-kbs" content=" , , "/>"#)
            .kb_linked_kbs
            .is_empty());
    }

    #[test]
    fn extracts_prompt_from_template() {
        let html = r#"<template id="kb-prompt">Generate a Rust hello world.</template>"#;
        let f = extract(html);
        assert_eq!(f.prompt.as_deref(), Some("Generate a Rust hello world."));
        assert_eq!(
            f.prompt_size_bytes,
            "Generate a Rust hello world.".len() as u32
        );
    }

    #[test]
    fn prompt_truncated_at_8kb_cap() {
        let huge = "x".repeat(20_000);
        let html = format!(r#"<template id="kb-prompt">{huge}</template>"#);
        let f = extract(&html);
        assert!(f.prompt.unwrap().len() <= PROMPT_MAX_BYTES);
    }

    #[test]
    fn prompt_cap_is_bytes_not_chars_for_multibyte() {
        // v0.7.1 P2 — the cap is BYTES. A prompt of 3-byte CJK chars used
        // to keep PROMPT_MAX_BYTES *chars* (~3x the byte cap). Truncation
        // must land on a char boundary (a non-boundary slice panics).
        let huge = "あ".repeat(20_000); // 3 bytes each → 60 KB
        let html = format!(r#"<template id="kb-prompt">{huge}</template>"#);
        let f = extract(&html);
        let prompt = f.prompt.unwrap();
        assert!(
            prompt.len() <= PROMPT_MAX_BYTES,
            "prompt is {} bytes, cap is {PROMPT_MAX_BYTES}",
            prompt.len()
        );
        // `String` is always valid UTF-8, but assert the truncation
        // didn't drop a partial char (which would have panicked anyway).
        assert!(prompt.chars().all(|c| c == 'あ'));
    }

    #[test]
    fn loc_buckets() {
        assert_eq!(LocBucket::from_bytes(0), LocBucket::Static);
        assert_eq!(LocBucket::from_bytes(500), LocBucket::Light);
        assert_eq!(LocBucket::from_bytes(5_000), LocBucket::Interactive);
        assert_eq!(LocBucket::from_bytes(50_000), LocBucket::Rich);
    }

    #[test]
    fn body_excerpt_skips_script_and_style() {
        let html = r#"
            <html><body>
                <p>visible 1</p>
                <script>alert('hidden')</script>
                <style>body { color: red; }</style>
                <p>visible 2</p>
            </body></html>
        "#;
        let f = extract(html);
        assert!(f.body_text_excerpt.contains("visible 1"));
        assert!(f.body_text_excerpt.contains("visible 2"));
        assert!(!f.body_text_excerpt.contains("alert"));
        assert!(!f.body_text_excerpt.contains("color: red"));
    }

    /// The sessions module's W0.6 sidecar-text tail block (`<section
    /// hidden>` holding `<details><summary>…</summary><pre>…</pre></details>`
    /// per agent) rides `body_text`'s `SKIP_TAGS` gap: `<section>`/
    /// `<details>`/`<pre>` aren't skipped, so a sidecar's raw text is
    /// visible to the indexer, while the sibling JSON digest block
    /// (`<script type="application/json">`) stays invisible — this is the
    /// whole rationale for choosing a `<section>` container over another
    /// `<script>` block.
    #[test]
    fn body_text_includes_sidecar_text_block_but_not_json_digest_blocks() {
        let base = "<html><body><h1>Session</h1><pre>{\"line\":\"ok\"}</pre></body></html>";

        let digest_only_token = "TOKEN-ONLY-IN-DIGEST-JSON";
        let digest = crate::sessions::render_subagents_block(&[crate::sessions::SubagentDigest {
            agent_id: digest_only_token.to_string(),
            files: vec![],
            tokens: 0,
            tool_calls: 0,
            errors: 0,
        }]);
        let with_digest = base.replacen("</body>", &format!("{digest}</body>"), 1);

        let sidecar_only_token = "TOKEN-ONLY-IN-SIDECAR-TEXT";
        let sidecar_block = crate::sessions::render_sidecar_text_block(&[(
            "a1".to_string(),
            format!("raw jsonl mentioning {sidecar_only_token} here\n"),
        )]);
        let with_sidecar = crate::sessions::replace_sidecar_text_block(&with_digest, sidecar_block);

        let f = extract(&with_sidecar);
        assert!(f.body.contains(sidecar_only_token), "{}", f.body);
        assert!(!f.body.contains(digest_only_token), "{}", f.body);
    }

    /// #11 W0.6 — `fields.code`'s selector (`pre code, pre`) is a plain CSS
    /// sweep of the WHOLE document, not a `<body>`-scoped walk like
    /// `body_text`'s `SKIP_TAGS` above: it doesn't care that the sidecar-text
    /// tail block's `<pre>` elements sit under a `<section hidden>` — a
    /// `hidden` attribute and a non-`<script>`/`<style>`/`<template>`
    /// ancestor are both invisible to a selector match. So a session capture
    /// envelope's `code` field ALREADY carries both the main transcript
    /// `<pre>` and every sidecar-text `<details><pre>` without the indexer
    /// doing anything extra — this is the empirical proof
    /// `indexer::prepare_doc`'s R1 branch leans on to KEEP the parser's
    /// `fields.code` verbatim (full-text BM25 evidence lane) instead of
    /// overwriting it with `sessions::extract_sidecar_text_block` alone.
    #[test]
    fn code_field_includes_both_main_transcript_and_sidecar_text_pre_blocks() {
        let main_only_token = "TOKEN-ONLY-IN-MAIN-PRE";
        let base = format!(
            "<html><body><h1>Session</h1><pre>{{\"line\":\"{main_only_token}\"}}</pre></body></html>"
        );

        let sidecar_only_token = "TOKEN-ONLY-IN-SIDECAR-PRE";
        let sidecar_block = crate::sessions::render_sidecar_text_block(&[(
            "a1".to_string(),
            format!("raw jsonl mentioning {sidecar_only_token} here\n"),
        )]);
        let with_sidecar = crate::sessions::replace_sidecar_text_block(&base, sidecar_block);

        let f = extract(&with_sidecar);
        assert!(
            f.code.contains(main_only_token),
            "main <pre> text missing from fields.code: {}",
            f.code
        );
        assert!(
            f.code.contains(sidecar_only_token),
            "sidecar-text <pre> text missing from fields.code: {}",
            f.code
        );
    }

    #[test]
    fn body_excerpt_capped_at_400_chars() {
        let html = format!("<body>{}</body>", "word ".repeat(200));
        let f = extract(&html);
        assert!(f.body_text_excerpt.len() <= BODY_EXCERPT_MAX_CHARS);
    }

    #[test]
    fn headings_collected_in_order() {
        let html = "<h1>Top</h1><h2>Sub</h2><details><summary>Open</summary></details>";
        let f = extract(html);
        assert!(f.headings.contains("Top"));
        assert!(f.headings.contains("Sub"));
        assert!(f.headings.contains("Open"));
    }

    // --- text_blocks (Track V prose extractor for diffing) ---------------

    #[test]
    fn text_blocks_one_line_per_block_skips_markup() {
        let html = r#"<body>
            <h1>Title</h1>
            <p>First   paragraph.</p>
            <script>ignored()</script>
            <ul><li>a</li><li>b</li></ul>
        </body>"#;
        assert_eq!(text_blocks(html), "Title\nFirst paragraph.\na\nb");
    }

    #[test]
    fn text_blocks_inline_runs_stay_on_one_line() {
        // Inline elements don't break the line; whitespace is collapsed.
        let html = "<body><p>a <strong>bold</strong> and <em>italic</em> word</p></body>";
        assert_eq!(text_blocks(html), "a bold and italic word");
    }

    #[test]
    fn text_blocks_stable_across_renesting_and_attrs() {
        // Same words, different wrapping/attributes/whitespace → same prose.
        let a = text_blocks("<body><p>Same words here</p></body>");
        let b = text_blocks(
            r#"<body><section><div class="x"><p>Same   words here</p></div></section></body>"#,
        );
        assert_eq!(a, b);
    }

    // --- Canon corpus fixtures (real artifacts) ------------------------------

    fn canon(rel: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/canon")
            .join(rel);
        std::fs::read_to_string(path).expect("canon fixture present")
    }

    #[test]
    fn canon_fullscreen_viz_extracts_borrow_title() {
        let html = canon("fullscreen-viz.html");
        let f = extract(&html);
        assert_eq!(f.title.as_deref(), Some("Visualizing the Borrow Checker"));
        assert_eq!(f.svg_count, 1);
        assert!(f.has_animation, "fullscreen-viz uses @keyframes shake");
        assert!(f.has_script);
    }

    #[test]
    fn canon_kitchen_sink_has_drag_and_details() {
        let html = canon("kitchen-sink.html");
        let f = extract(&html);
        assert!(f.has_drag, "kitchen-sink has draggable=\"true\" lis");
        assert!(f.has_details);
        assert!(f.has_script);
        // kitchen-sink.html has ~7 KB of inline CSS — Interactive bucket.
        assert_eq!(f.css_loc, LocBucket::Interactive);
    }

    #[test]
    fn canon_multi_page_extracts_title() {
        let html = canon("multi-page.html");
        let f = extract(&html);
        assert_eq!(f.title.as_deref(), Some("A Field Guide to Rust Errors"));
    }

    #[test]
    fn canon_pm_summary_extracts_postmortem_title() {
        let html = canon("pm/00-summary.html");
        let f = extract(&html);
        assert_eq!(f.title.as_deref(), Some("INC-0315 · Summary"));
    }

    #[test]
    fn headings_in_order_walks_h1_through_h6() {
        let html = "<h1>Top</h1><h2>One</h2><h3>One.A</h3><h2>Two</h2>";
        let h = headings_in_order(html);
        assert_eq!(
            h,
            vec![
                (1, "Top".to_string()),
                (2, "One".to_string()),
                (3, "One.A".to_string()),
                (2, "Two".to_string()),
            ]
        );
    }

    #[test]
    fn headings_in_order_skips_empty() {
        let html = "<h1></h1><h2>real</h2><h3>   </h3>";
        let h = headings_in_order(html);
        assert_eq!(h, vec![(2, "real".to_string())]);
    }

    #[test]
    fn headings_in_order_handles_no_headings() {
        assert!(headings_in_order("<p>no headings</p>").is_empty());
    }

    // --- extract_links (F1) ----------------------------------------------

    const SFX: &str = crate::iframe::DEFAULT_HOST_SUFFIX;

    #[test]
    fn extract_links_picks_up_subdomain_urls() {
        let html = r#"
            <html><body>
              <a href="http://kitchen-sink.artifacts.localhost:4737/">link</a>
              <a href="https://multi-page.artifacts.localhost/">https</a>
              <a href="//fullscreen-viz.artifacts.localhost/some/path">scheme-relative</a>
            </body></html>
        "#;
        let ids = extract_links(html, SFX);
        assert_eq!(ids, vec!["kitchen-sink", "multi-page", "fullscreen-viz"]);
    }

    // invariant:7 host-suffix
    #[test]
    fn extract_links_picks_up_production_suffix() {
        // Cross-artifact links inside HTML authored on a production
        // deployment use the configured suffix.
        let html = r#"
            <a href="https://abc.artifacts.example.com/">prod</a>
            <a href="http://localhost-shaped.artifacts.localhost/">dev-shaped</a>
        "#;
        let ids = extract_links(html, ".artifacts.example.com");
        // Only the prod-suffixed link matches; the dev one is ignored
        // (cross-suffix references are a different deployment's links).
        assert_eq!(ids, vec!["abc"]);
    }

    #[test]
    fn extract_links_picks_up_spa_permalinks() {
        // Path-form permalinks (the documented authoring shape) resolve
        // to the artifact id by hashing the FULL source-relative path —
        // query and fragment stripped first. Id-form (12-hex) permalinks
        // pass through verbatim.
        let html = r##"
            <a href="/a/canon/notes/deploy.html">nested path</a>
            <a href="/a/work/multi-page.html?ref=foo">with query</a>
            <a href="/a/canon/START-HERE.md#sec1">markdown, with frag</a>
            <a href="/a/canon/0123456789ab">id form</a>
        "##;
        let ids = extract_links(html, SFX);
        assert_eq!(
            ids,
            vec![
                crate::ids::ArtifactId::from_path("notes/deploy.html")
                    .as_str()
                    .to_string(),
                crate::ids::ArtifactId::from_path("multi-page.html")
                    .as_str()
                    .to_string(),
                crate::ids::ArtifactId::from_path("START-HERE.md")
                    .as_str()
                    .to_string(),
                "0123456789ab".to_string(),
            ]
        );
        // The nested path hashes as a WHOLE — not its first segment (the
        // pre-2026-07-03 nth(1) bug that produced dead edge keys like "notes").
        assert_ne!(ids[0], crate::ids::ArtifactId::from_path("notes").as_str());
        assert_ne!(ids[0], "notes");
    }

    #[test]
    fn extract_links_percent_decodes_path_form() {
        let html = r##"<a href="/a/canon/my%20notes/plan.html">enc</a>"##;
        let ids = extract_links(html, SFX);
        assert_eq!(
            ids,
            vec![crate::ids::ArtifactId::from_path("my notes/plan.html")
                .as_str()
                .to_string()]
        );
    }

    #[test]
    fn extract_links_dedupes() {
        // The same source-relative path linked under two kb segments is
        // one target, and the subdomain form carrying that path's hashed
        // id collapses with them.
        let id = crate::ids::ArtifactId::from_path("foo.html");
        let html = format!(
            r#"
            <a href="/a/canon/foo.html">one</a>
            <a href="/a/other/foo.html">same path, other kb</a>
            <a href="http://{id}.artifacts.localhost/">subdomain, hashed id</a>
            "#,
            id = id.as_str()
        );
        let ids = extract_links(&html, SFX);
        assert_eq!(ids, vec![id.as_str().to_string()]);
    }

    #[test]
    fn extract_links_drops_external_and_local_anchors() {
        let html = r##"
            <a href="https://example.com/path">external</a>
            <a href="mailto:foo@bar">mail</a>
            <a href="#section">in-doc</a>
            <a href="javascript:void(0)">js</a>
            <a href="/api/identity">api</a>
        "##;
        assert!(extract_links(html, SFX).is_empty());
    }

    #[test]
    fn extract_links_rejects_path_traversal_in_id() {
        let html = r##"
            <a href="/a/canon/..%2Fevil">traversal</a>
            <a href="http://..bad.artifacts.localhost/">subdomain traversal</a>
        "##;
        // "..%2Fevil" percent-decodes to "../evil"; the ".." segment is
        // a traversal shape no indexed artifact can carry → no-match.
        let ids = extract_links(html, SFX);
        assert!(ids.is_empty(), "expected no links, got {ids:?}");
    }

    #[test]
    fn extract_links_handles_no_anchors() {
        assert!(extract_links("<p>plain text</p>", SFX).is_empty());
    }

    // --- extract_link_hrefs (cross-artifact relative-path resolution) ----

    #[test]
    fn extract_link_hrefs_keeps_relative_and_root_paths() {
        let html = r##"
            <a href="../../incidents/checks/check-2026-05-13.html">up-tree</a>
            <a href="sibling.html">sibling</a>
            <a href="./also-sibling.html">explicit sibling</a>
            <a href="sub/nested.html">child</a>
            <a href="/root-anchored.html">root</a>
        "##;
        let hrefs = extract_link_hrefs(html);
        assert_eq!(
            hrefs,
            vec![
                "../../incidents/checks/check-2026-05-13.html",
                "sibling.html",
                "./also-sibling.html",
                "sub/nested.html",
                "/root-anchored.html",
            ]
        );
    }

    #[test]
    fn extract_link_hrefs_drops_absolute_external_and_pseudo_schemes() {
        let html = r##"
            <a href="https://example.com/x">external</a>
            <a href="http://example.com/x">external http</a>
            <a href="//example.com/x">scheme-relative</a>
            <a href="mailto:foo@bar">mail</a>
            <a href="tel:+1234">tel</a>
            <a href="javascript:alert(1)">js</a>
            <a href="#section">frag</a>
            <a href="">empty</a>
            <a>no href</a>
        "##;
        assert!(extract_link_hrefs(html).is_empty());
    }

    #[test]
    fn extract_link_hrefs_preserves_order_and_keeps_duplicates() {
        // The indexer wants every relative href so it can resolve each
        // to a target path; dedup happens after path resolution, not
        // here, because two distinct hrefs can canonicalise to the same
        // file (`./a.html` vs `a.html`).
        let html = r##"
            <a href="a.html">1</a>
            <a href="b.html">2</a>
            <a href="a.html">3</a>
        "##;
        let hrefs = extract_link_hrefs(html);
        assert_eq!(hrefs, vec!["a.html", "b.html", "a.html"]);
    }

    #[test]
    fn extract_link_hrefs_subdomain_urls_excluded() {
        // Subdomain URLs are already handled by extract_links — keeping
        // them out of extract_link_hrefs prevents the indexer from
        // double-counting the same edge.
        let html = r##"
            <a href="http://abc123.artifacts.localhost:4000/">artifact subdomain</a>
            <a href="/a/foo/abc123">spa permalink</a>
            <a href="../relative.html">relative</a>
        "##;
        let hrefs = extract_link_hrefs(html);
        // Note: `/a/foo/abc123` is kept here — it's a root-anchored
        // path. The indexer drops it because canonicalising under the
        // source root won't match (it's a SPA route, not a file). The
        // de-duplication against extract_links happens in the indexer.
        assert_eq!(hrefs, vec!["/a/foo/abc123", "../relative.html"]);
    }
}
