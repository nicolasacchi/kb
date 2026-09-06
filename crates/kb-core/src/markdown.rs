//! First-class Markdown (`.md`) artifacts. ONE render pipeline shared by the
//! indexer (metadata + edges), the artifact serve path, `kb get`, and the
//! static-share exporter — so heading text + structure never drift between
//! the index and the served DOM.
//!
//! Security (root CLAUDE.md invariant): the kb-prompt for a markdown artifact
//! comes ONLY from an inline `<template id="kb-prompt">` — preserved by raw-
//! HTML passthrough (`render.r#unsafe`) and stripped by the outbound scrub
//! (which targets exactly that element). There is **no** frontmatter
//! `prompt:` key, so a prompt can never leak past the element-only scrub on
//! public serve / static share. Rendered markdown is served solely from the
//! CSP-wrapped, scrubbed `text/html` branch of the artifact route.
//!
//! Heading ids: we deliberately do NOT enable comrak's `header_id_prefix`.
//! comrak emits that id on an inner `<a class="anchor">`, not on the `<h>`
//! element, which would mismatch the in-iframe runtime's `kb-h-<slug>` scheme
//! (it ids the `<h>` itself) and produce double ids. Instead the runtime owns
//! heading ids uniformly with hand-authored HTML artifacts; Chapter comment
//! anchors match on heading TEXT, which this shared config keeps stable.

use comrak::options::Plugins;
use comrak::plugins::syntect::SyntectAdapter;
use comrak::{markdown_to_html_with_plugins, Options};

/// Inlined reading-theme stylesheet (kb tokens, serif prose, mono code, the
/// "markdown" accent bar). Self-contained so the cross-origin iframe needs no
/// parent-SPA assets — mirrors `session_render.css`.
const PROSE_CSS: &str = include_str!("markdown_prose.css");

/// A light syntect theme from `ThemeSet::load_defaults()` that suits the
/// reading theme; emits self-contained inline-styled highlighted code.
const SYNTECT_THEME: &str = "InspiredGitHub";

/// Flat-frontmatter fields (a YAML subset: scalars + comma / `[a, b]` lists),
/// mirroring the HTML `<meta name="kb-*">` conventions. NOTE: there is no
/// `prompt` field by design — see the module security note.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub kb_category: Option<String>,
    pub kb_tags: Vec<String>,
    pub kb_status: Option<String>,
    pub kb_severity: Option<String>,
    pub kb_salience: Option<f32>,
    pub kb_decay: Option<String>,
    pub kb_supersedes: Option<String>,
    pub kb_session: Option<String>,
    pub kb_global: bool,
    pub kb_linked_kbs: Vec<String>,
}

/// Apply the single shared comrak config used at every render site.
fn apply_kb_opts(o: &mut Options) {
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.footnotes = true;
    o.extension.superscript = true;
    o.extension.description_lists = true;
    // Raw HTML passes through (inline `<template id="kb-prompt">`, etc.) — same
    // trust model as hand-authored HTML artifacts; only ever served inside the
    // sandboxed, CSP-wrapped, scrubbed iframe branch.
    o.render.r#unsafe = true;
}

/// The single shared comrak [`Options`] used at every render AND parse site.
/// `notes::scan_tasks` parses the body with these exact options to locate GFM
/// task lines via the AST, so the task set it finds is byte-identical to what
/// [`render_fragment`] emits as `<input type=checkbox>` (and to what
/// remark-gfm renders client-side). Task-item source positions
/// (`comrak::nodes::NodeTaskItem::symbol_sourcepos`) are 1-based **byte**
/// columns — `parse.sourcepos_chars` stays at its default (`false`).
pub fn kb_options() -> Options<'static> {
    let mut o = Options::default();
    apply_kb_opts(&mut o);
    o
}

/// Render a Markdown body (frontmatter already stripped) to an HTML fragment
/// with GFM + server-side syntect highlighting. The single render entry that
/// every caller funnels through, so the rendered structure is identical
/// across the index, serve, and share paths. Obsidian-style callouts
/// (`> [!type] …` blockquotes) are rewritten to styled `<div class="kb-callout
/// kb-callout--type">` here; the SPA's native note view does the equivalent
/// via a rehype pass. NB: this runs in the RENDER path only — `kb_options`/
/// `notes::scan_tasks` parse the raw body, so a task inside a callout keeps its
/// document-order ordinal (invariant #16 unaffected).
pub fn render_fragment(body_md: &str) -> String {
    let rewritten = rewrite_callouts(body_md);
    let o = kb_options();
    let adapter = SyntectAdapter::new(Some(SYNTECT_THEME));
    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&adapter);
    markdown_to_html_with_plugins(&rewritten, &o, &plugins)
}

/// Render an UNTRUSTED comment/reply body (GFM markdown) to an HTML
/// fragment — used by the static-share exporter (Y-track) to bake comment
/// threads into a public page. Unlike [`render_fragment`] (for authored
/// `.md` artifacts), this sets `render.unsafe = false`, so raw HTML in a
/// comment is ESCAPED rather than passed through: a comment can't inject
/// markup into the published page. This mirrors the SPA's no-`rehype-raw`
/// posture for comment bodies. No callout rewriting (callouts emit raw
/// `<div>`s that depend on `render.unsafe`, which is off here); a rewritten
/// `attachment:` ref is an ordinary relative `![](…)`/`[](…)` and renders
/// fine. The caller rewrites refs (`rewrite_attachment_refs`) BEFORE this.
pub fn render_comment_fragment(body_md: &str) -> String {
    let mut o = kb_options();
    o.render.r#unsafe = false;
    let adapter = SyntectAdapter::new(Some(SYNTECT_THEME));
    let mut plugins = Plugins::default();
    plugins.render.codefence_syntax_highlighter = Some(&adapter);
    markdown_to_html_with_plugins(body_md, &o, &plugins)
}

/// Rewrite Obsidian callout blockquotes into class-tagged `<div>`s the prose
/// CSS styles. A callout is a blockquote whose first line is `[!type]` with an
/// optional fold marker (`+`/`-`) and title:
///
/// ```text
/// > [!warning] Heads up
/// > body **markdown** still renders
/// ```
///
/// becomes (relying on `render.unsafe` + the blank-line rule so comrak still
/// parses the de-quoted body as Markdown):
///
/// ```text
/// <div class="kb-callout kb-callout--warning">
/// <div class="kb-callout__title">Heads up</div>
///
/// body **markdown** still renders
///
/// </div>
/// ```
///
/// Plain blockquotes (no `[!type]` first line) pass through untouched, and a
/// body with no callout anywhere returns byte-identical (the fast path).
fn rewrite_callouts(md: &str) -> String {
    if !md.lines().any(|l| callout_header(l).is_some()) {
        return md.to_string();
    }
    let lines: Vec<&str> = md.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 8);
    let mut i = 0;
    while i < lines.len() {
        match callout_header(lines[i]) {
            Some((ctype, title)) => {
                // Consume the contiguous blockquote run.
                let mut j = i + 1;
                while j < lines.len() && lines[j].starts_with('>') {
                    j += 1;
                }
                let title_text = if title.is_empty() {
                    capitalize(&ctype)
                } else {
                    title
                };
                out.push(format!(
                    "<div class=\"kb-callout kb-callout--{}\">",
                    ctype.to_ascii_lowercase()
                ));
                out.push(format!(
                    "<div class=\"kb-callout__title\">{}</div>",
                    html_escape(&title_text)
                ));
                out.push(String::new()); // blank line → body parsed as Markdown
                for line in &lines[i + 1..j] {
                    out.push(dequote(line).to_string());
                }
                out.push(String::new());
                out.push("</div>".to_string());
                i = j;
            }
            None => {
                out.push(lines[i].to_string());
                i += 1;
            }
        }
    }
    out.join("\n")
}

/// Strip one `>` blockquote marker (and a single following space) from a line.
fn dequote(line: &str) -> &str {
    let l = line.strip_prefix('>').unwrap_or(line);
    l.strip_prefix(' ').unwrap_or(l)
}

/// If `line` opens an Obsidian callout (`> [!type] title`), return
/// `(type, title)` — title may be empty. A fold marker (`+`/`-`) flush against
/// `]` is consumed and ignored (kb doesn't render collapsible state).
fn callout_header(line: &str) -> Option<(String, String)> {
    if !line.starts_with('>') {
        return None;
    }
    let content = dequote(line).trim_start();
    let rest = content.strip_prefix("[!")?;
    let close = rest.find(']')?;
    let ctype = &rest[..close];
    if ctype.is_empty() || !ctype.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    // The fold marker sits flush against `]` with no space; only strip it in
    // that position so a title that legitimately starts with `-` survives.
    let raw_after = &rest[close + 1..];
    let after = raw_after.strip_prefix(['+', '-']).unwrap_or(raw_after);
    Some((ctype.to_string(), after.trim().to_string()))
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Split a leading `---\n … \n---` frontmatter block off the source, returning
/// the parsed (flat) frontmatter + the remaining Markdown body. Only a block
/// at the very start counts; a bare leading `---` with no closing fence is
/// treated as body (a thematic break), not frontmatter.
pub fn parse_frontmatter(src: &str) -> (FrontMatter, &str) {
    let mut fm = FrontMatter::default();
    // A leading UTF-8 BOM (common from Windows / "save as UTF-8" editors) would
    // otherwise defeat the `---` prefix test, silently discarding all
    // frontmatter and dumping it as visible body. Strip it first.
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let after_open = match src
        .strip_prefix("---\n")
        .or_else(|| src.strip_prefix("---\r\n"))
    {
        Some(rest) => rest,
        None => return (fm, src),
    };
    // Locate the closing `---` fence (its own line).
    let mut offset = 0usize;
    let mut block: Option<(usize, usize)> = None; // (block_end_exclusive, body_start)
    for line in after_open.split_inclusive('\n') {
        // `trim()` (not just newline trim) so a stray trailing space/tab on the
        // closing fence — invisible, easy to introduce — still closes the block.
        if line.trim() == "---" {
            block = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    let Some((block_end, body_start)) = block else {
        // No closing fence → the leading `---` is a thematic break, not fm.
        return (fm, src);
    };
    for raw in after_open[..block_end].lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let val = val.trim().trim_matches('"');
        if val.is_empty() {
            continue;
        }
        match key.trim() {
            "title" => fm.title = Some(val.to_string()),
            "kb-category" => fm.kb_category = Some(val.to_string()),
            "kb-tags" => fm.kb_tags = split_list(val),
            "kb-status" => fm.kb_status = Some(val.to_string()),
            "kb-severity" => fm.kb_severity = Some(val.to_string()),
            "kb-salience" => {
                // Reject NaN/inf — NaN survives `clamp` and would silently drop
                // the memory from recall (`score > floor` is false for NaN).
                fm.kb_salience = val
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(|v| v.clamp(0.0, 1.0));
            }
            "kb-decay" => fm.kb_decay = Some(val.to_string()),
            "kb-supersedes" => fm.kb_supersedes = Some(val.to_string()),
            "kb-session" => fm.kb_session = Some(val.to_string()),
            "kb-global" => fm.kb_global = val.eq_ignore_ascii_case("true"),
            "kb-linked-kbs" => fm.kb_linked_kbs = split_list(val),
            _ => {}
        }
    }
    (fm, &after_open[body_start..])
}

/// Parse a flat list value: `a, b, c` or `[a, b, c]`.
fn split_list(val: &str) -> Vec<String> {
    val.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Set / insert / remove a flat-frontmatter `key` in a Markdown source,
/// preserving the body and every other frontmatter line byte-for-byte —
/// the write-side inverse of [`parse_frontmatter`]. Backs the
/// `PATCH …/artifacts/{id}/meta` route for `.md` artifacts (the HTML
/// `<meta>` editor's counterpart).
///
/// - `value == Some(v)`: replace the key's line (collapsing any duplicates
///   to one), or insert `key: v` just before the closing fence when the
///   key is absent, or prepend a fresh `---\nkey: v\n---\n` block when the
///   source has no frontmatter at all.
/// - `value == None`: remove the key's line(s); a no-op when absent or when
///   the source has no frontmatter.
///
/// List values (e.g. `kb-tags`) should be pre-joined `a, b` by the caller;
/// `split_list` round-trips that form. A leading UTF-8 BOM is preserved.
/// A bare leading `---` with no closing fence is a thematic break (not
/// frontmatter), matching `parse_frontmatter` — editing then prepends a
/// new block, leaving the break in the body.
pub fn set_frontmatter_field(src: &str, key: &str, value: Option<&str>) -> String {
    let (bom, rest) = match src.strip_prefix('\u{feff}') {
        Some(r) => ("\u{feff}", r),
        None => ("", src),
    };
    let open = if rest.starts_with("---\n") {
        Some("---\n")
    } else if rest.starts_with("---\r\n") {
        Some("---\r\n")
    } else {
        None
    };
    if let Some(open) = open {
        let after_open = &rest[open.len()..];
        // Locate the closing `---` fence (its own line), mirroring
        // `parse_frontmatter`.
        let mut offset = 0usize;
        let mut fence_start = None;
        for line in after_open.split_inclusive('\n') {
            if line.trim() == "---" {
                fence_start = Some(offset);
                break;
            }
            offset += line.len();
        }
        if let Some(fence_start) = fence_start {
            let keys_region = &after_open[..fence_start];
            let tail = &after_open[fence_start..]; // closing fence + body, verbatim
            let crlf = open.ends_with("\r\n");
            let new_keys = edit_keys_region(keys_region, key, value, crlf);
            return format!("{bom}{open}{new_keys}{tail}");
        }
        // Leading `---` with no closing fence → not frontmatter; fall
        // through to prepend a fresh block.
    }
    match value {
        Some(v) => format!("{bom}---\n{key}: {v}\n---\n{rest}"),
        None => src.to_string(),
    }
}

/// Rewrite the `key: value` lines of a frontmatter block. Non-matching
/// lines (other keys, `#` comments, blanks) are preserved verbatim;
/// duplicate `key` lines collapse to the single edited/inserted one so the
/// value `parse_frontmatter` reads (last-wins) is unambiguous.
fn edit_keys_region(keys_region: &str, key: &str, value: Option<&str>, crlf: bool) -> String {
    let is_match = |line: &str| -> bool {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            return false;
        }
        t.split_once(':')
            .map(|(k, _)| k.trim() == key)
            .unwrap_or(false)
    };
    let mut out = String::with_capacity(keys_region.len() + 32);
    let mut emitted = false;
    for line in keys_region.split_inclusive('\n') {
        if is_match(line) {
            if let Some(v) = value {
                if !emitted {
                    let eol = if line.ends_with("\r\n") {
                        "\r\n"
                    } else if line.ends_with('\n') {
                        "\n"
                    } else {
                        ""
                    };
                    out.push_str(&format!("{key}: {v}{eol}"));
                    emitted = true;
                }
                // else: drop duplicate key lines
            }
            // value == None: drop the line entirely
        } else {
            out.push_str(line);
        }
    }
    if let Some(v) = value {
        if !emitted {
            let eol = if crlf { "\r\n" } else { "\n" };
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(eol);
            }
            out.push_str(&format!("{key}: {v}{eol}"));
        }
    }
    out
}

/// First `# H1` text in the body, if any — the title fallback when the
/// frontmatter carries no `title`.
pub fn first_h1(body_md: &str) -> Option<String> {
    body_md.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
    })
}

/// Full serve-time render: frontmatter + body → a self-contained, styled HTML
/// document (reading theme + the "markdown" accent bar). The result enters the
/// existing artifact serve pipeline (scrub → probe → runtime → annotator)
/// exactly like a hand-authored HTML artifact.
pub fn render_page(src: &str) -> String {
    let (fm, body) = parse_frontmatter(src);
    let title = fm
        .title
        .or_else(|| first_h1(body))
        .unwrap_or_else(|| "Untitled".to_string());
    let fragment = render_fragment(body);

    let mut out = String::with_capacity(fragment.len() + PROSE_CSS.len() + 2048);
    out.push_str("<!doctype html>\n<html lang=\"en\"><head>\n");
    out.push_str(r#"<meta charset="utf-8">"#);
    out.push_str(r#"<meta name="viewport" content="width=device-width,initial-scale=1">"#);
    out.push_str("\n<title>");
    push_escaped(&mut out, &title);
    out.push_str("</title>\n<style>\n");
    out.push_str(PROSE_CSS);
    out.push_str("\n</style>\n</head>\n");
    out.push_str(r#"<body class="kb-md-prose">"#);
    out.push_str(
        r#"<div class="kb-md-bar" aria-hidden="true"><span class="kb-md-bar__dot"></span>markdown</div>"#,
    );
    out.push_str(r#"<main class="kb-md-doc">"#);
    out.push_str(&fragment);
    out.push_str("</main></body></html>");
    out
}

/// Minimal HTML-text escape for the `<title>`.
fn push_escaped(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_parses_flat_fields_and_lists() {
        let src = "---\ntitle: My Spec\nkb-category: design\nkb-tags: rust, async, errors\nkb-salience: 0.8\nkb-global: true\nkb-linked-kbs: [memory, kb-other]\n---\n# Body\n\ntext\n";
        let (fm, body) = parse_frontmatter(src);
        assert_eq!(fm.title.as_deref(), Some("My Spec"));
        assert_eq!(fm.kb_category.as_deref(), Some("design"));
        assert_eq!(fm.kb_tags, vec!["rust", "async", "errors"]);
        assert_eq!(fm.kb_salience, Some(0.8));
        assert!(fm.kb_global);
        assert_eq!(fm.kb_linked_kbs, vec!["memory", "kb-other"]);
        assert!(body.starts_with("# Body"));
    }

    #[test]
    fn frontmatter_rejects_non_finite_salience() {
        // NaN must NOT become Some(NaN) — it survives clamp and would silently
        // drop the memory from recall. inf is likewise rejected.
        assert_eq!(
            parse_frontmatter("---\nkb-salience: NaN\n---\nbody\n")
                .0
                .kb_salience,
            None
        );
        assert_eq!(
            parse_frontmatter("---\nkb-salience: inf\n---\nbody\n")
                .0
                .kb_salience,
            None
        );
        // A finite value still clamps to 0..1.
        assert_eq!(
            parse_frontmatter("---\nkb-salience: 1.7\n---\nb\n")
                .0
                .kb_salience,
            Some(1.0)
        );
    }

    #[test]
    fn frontmatter_survives_bom_and_trailing_ws_fence() {
        // A leading BOM must not defeat frontmatter detection.
        let (fm, body) = parse_frontmatter("\u{feff}---\ntitle: T\n---\n# H\n");
        assert_eq!(fm.title.as_deref(), Some("T"));
        assert!(body.starts_with("# H"));
        // A trailing space on the closing fence still closes the block.
        let (fm, body) = parse_frontmatter("---\ntitle: T2\n--- \n# H2\n");
        assert_eq!(fm.title.as_deref(), Some("T2"));
        assert!(body.starts_with("# H2"));
    }

    #[test]
    fn leading_hr_without_close_is_not_frontmatter() {
        let src = "---\njust a thematic break then text\n";
        let (fm, body) = parse_frontmatter(src);
        assert_eq!(fm, FrontMatter::default());
        assert_eq!(body, src);
    }

    #[test]
    fn no_frontmatter_returns_whole_body() {
        let src = "# Title\n\nsome text\n";
        let (fm, body) = parse_frontmatter(src);
        assert_eq!(fm, FrontMatter::default());
        assert_eq!(body, src);
    }

    #[test]
    fn set_fm_replaces_existing_list() {
        let src = "---\ntitle: T\nkb-tags: a, b\n---\n# Body\n";
        let out = set_frontmatter_field(src, "kb-tags", Some("c, d"));
        assert_eq!(out, "---\ntitle: T\nkb-tags: c, d\n---\n# Body\n");
        assert_eq!(parse_frontmatter(&out).0.kb_tags, vec!["c", "d"]);
    }

    #[test]
    fn set_fm_replaces_scalar() {
        let src = "---\nkb-category: notes\n---\nb\n";
        let out = set_frontmatter_field(src, "kb-category", Some("research"));
        assert_eq!(
            parse_frontmatter(&out).0.kb_category.as_deref(),
            Some("research")
        );
    }

    #[test]
    fn set_fm_inserts_missing_key_before_fence() {
        let src = "---\ntitle: T\n---\n# Body\n";
        let out = set_frontmatter_field(src, "kb-tags", Some("x, y"));
        assert_eq!(out, "---\ntitle: T\nkb-tags: x, y\n---\n# Body\n");
        assert_eq!(parse_frontmatter(&out).0.kb_tags, vec!["x", "y"]);
    }

    #[test]
    fn set_fm_creates_block_when_absent() {
        let src = "# Just a heading\n\ntext\n";
        let out = set_frontmatter_field(src, "kb-tags", Some("a, b"));
        assert_eq!(out, "---\nkb-tags: a, b\n---\n# Just a heading\n\ntext\n");
        assert_eq!(parse_frontmatter(&out).0.kb_tags, vec!["a", "b"]);
    }

    #[test]
    fn set_fm_removes_key() {
        let src = "---\ntitle: T\nkb-tags: a, b\n---\nbody\n";
        let out = set_frontmatter_field(src, "kb-tags", None);
        assert_eq!(out, "---\ntitle: T\n---\nbody\n");
        assert!(parse_frontmatter(&out).0.kb_tags.is_empty());
    }

    #[test]
    fn set_fm_remove_absent_is_noop() {
        let src = "---\ntitle: T\n---\nbody\n";
        assert_eq!(set_frontmatter_field(src, "kb-tags", None), src);
        // No frontmatter at all + remove → unchanged.
        let plain = "# H\ntext\n";
        assert_eq!(set_frontmatter_field(plain, "kb-tags", None), plain);
    }

    #[test]
    fn set_fm_preserves_bom() {
        let src = "\u{feff}---\nkb-tags: a\n---\nbody\n";
        let out = set_frontmatter_field(src, "kb-tags", Some("b"));
        assert_eq!(out, "\u{feff}---\nkb-tags: b\n---\nbody\n");
    }

    #[test]
    fn set_fm_preserves_crlf() {
        let src = "---\r\ntitle: T\r\nkb-tags: a\r\n---\r\nbody\r\n";
        let out = set_frontmatter_field(src, "kb-tags", Some("b, c"));
        assert_eq!(out, "---\r\ntitle: T\r\nkb-tags: b, c\r\n---\r\nbody\r\n");
    }

    #[test]
    fn set_fm_leading_hr_not_mistaken_for_frontmatter() {
        // No closing fence → the leading `---` is a thematic break; editing
        // prepends a fresh block and leaves the break in the body.
        let src = "---\nthematic break, then prose\n";
        let out = set_frontmatter_field(src, "kb-tags", Some("a"));
        assert_eq!(
            out,
            "---\nkb-tags: a\n---\n---\nthematic break, then prose\n"
        );
        let (fm, body) = parse_frontmatter(&out);
        assert_eq!(fm.kb_tags, vec!["a"]);
        assert!(body.starts_with("---\nthematic break"));
    }

    #[test]
    fn set_fm_preserves_comments_and_other_keys() {
        let src = "---\n# a comment\ntitle: T\nkb-tags: a\nkb-status: draft\n---\nbody\n";
        let out = set_frontmatter_field(src, "kb-tags", Some("z"));
        assert!(out.contains("# a comment"));
        assert!(out.contains("kb-status: draft"));
        assert!(out.contains("kb-tags: z"));
    }

    #[test]
    fn set_fm_collapses_duplicate_keys() {
        let src = "---\nkb-tags: a\nkb-tags: b\n---\nbody\n";
        let out = set_frontmatter_field(src, "kb-tags", Some("c"));
        assert_eq!(out.matches("kb-tags:").count(), 1);
        assert_eq!(parse_frontmatter(&out).0.kb_tags, vec!["c"]);
    }

    #[test]
    fn first_h1_is_title_fallback() {
        assert_eq!(
            first_h1("intro\n# The Heading\nmore"),
            Some("The Heading".to_string())
        );
        assert_eq!(first_h1("no heading here"), None);
    }

    #[test]
    fn render_fragment_does_gfm_table_and_tasklist() {
        let md = "| a | b |\n|---|---|\n| 1 | 2 |\n\n- [x] done\n- [ ] todo\n";
        let html = render_fragment(md);
        assert!(html.contains("<table>"), "GFM table: {html}");
        assert!(html.contains("type=\"checkbox\""), "task list: {html}");
    }

    #[test]
    fn render_fragment_highlights_code() {
        let md = "```rust\nfn main() {}\n```\n";
        let html = render_fragment(md);
        // syntect emits inline-styled spans inside a <pre>.
        assert!(html.contains("<pre"), "code block present: {html}");
        assert!(
            html.contains("style=\"color"),
            "syntect inline styles: {html}"
        );
    }

    #[test]
    fn render_fragment_styles_obsidian_callout() {
        let md = "> [!warning] Heads up\n> be careful **here**\n";
        let html = render_fragment(md);
        assert!(
            html.contains(r#"class="kb-callout kb-callout--warning""#),
            "callout div + type class: {html}"
        );
        assert!(
            html.contains(r#"class="kb-callout__title""#),
            "title div: {html}"
        );
        assert!(html.contains("Heads up"), "explicit title: {html}");
        // The de-quoted body still renders as Markdown (the blank-line rule).
        assert!(
            html.contains("<strong>here</strong>"),
            "body parsed as markdown: {html}"
        );
        assert!(!html.contains("[!warning]"), "marker stripped: {html}");
    }

    #[test]
    fn callout_default_title_is_capitalised_type() {
        let html = render_fragment("> [!tip]\n> do this\n");
        assert!(html.contains("kb-callout--tip"), "{html}");
        assert!(
            html.contains(">Tip<"),
            "default title = capitalised type: {html}"
        );
    }

    #[test]
    fn callout_type_alias_kept_verbatim_lowercased() {
        // Alias/unknown types keep their (lowercased) class so the CSS base
        // style still applies; `[!CAUTION]` → `kb-callout--caution`.
        let html = render_fragment("> [!CAUTION] x\n> y\n");
        assert!(html.contains("kb-callout--caution"), "{html}");
    }

    #[test]
    fn plain_blockquote_is_not_a_callout() {
        let md = "> just an ordinary quote\n";
        let html = render_fragment(md);
        assert!(html.contains("<blockquote>"), "stays a blockquote: {html}");
        assert!(!html.contains("kb-callout"), "no callout wrapping: {html}");
    }

    #[test]
    fn body_without_callout_is_unchanged() {
        // Fast-path passthrough — no callout anywhere → byte-identical source.
        let md = "# Title\n\nA paragraph.\n\n> a quote\n";
        assert_eq!(rewrite_callouts(md), md);
    }

    #[test]
    fn callout_title_is_html_escaped() {
        let html = render_fragment("> [!note] <b>x</b> & y\n> body\n");
        assert!(
            html.contains("&lt;b&gt;x&lt;/b&gt; &amp; y"),
            "title escaped, no raw tag injection: {html}"
        );
    }

    #[test]
    fn render_fragment_passes_through_inline_template() {
        // The inline kb-prompt template must survive render (raw-HTML
        // passthrough) so the outbound scrub can strip it later.
        let md = "# Doc\n\n<template id=\"kb-prompt\">secret prompt</template>\n";
        let html = render_fragment(md);
        assert!(
            html.contains(r#"<template id="kb-prompt">"#),
            "template preserved: {html}"
        );
    }

    #[test]
    fn render_page_is_self_contained_with_bar_and_shell() {
        let src = "---\ntitle: Hello\n---\n# Hello\n\nworld\n";
        let page = render_page(src);
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<title>Hello</title>"));
        assert!(page.contains("kb-md-bar"), "accent bar present");
        assert!(page.contains(r#"class="kb-md-doc""#));
        assert!(page.contains("<h1>Hello</h1>") || page.contains("Hello</h1>"));
        // Frontmatter is consumed, not rendered as body text.
        assert!(!page.contains("title: Hello"));
    }

    #[test]
    fn render_page_title_falls_back_to_h1_then_untitled() {
        assert!(render_page("# From H1\n\nx").contains("<title>From H1</title>"));
        assert!(render_page("just text, no heading").contains("<title>Untitled</title>"));
    }

    #[test]
    fn render_fragment_is_deterministic() {
        let md = "# A\n## B\n## B\ntext `code` and **bold**\n";
        assert_eq!(render_fragment(md), render_fragment(md));
    }

    #[test]
    fn render_comment_fragment_escapes_raw_html() {
        // Comment bodies are UNTRUSTED: raw HTML must be escaped, not passed
        // through, so a comment can't inject markup into a public shared
        // page. (render_fragment, for authored artifacts, would pass it.)
        let html = render_comment_fragment("hi <script>alert(1)</script> **bold**");
        // comrak with render.unsafe=false strips raw HTML entirely (emits a
        // `<!-- raw HTML omitted -->` placeholder) — even stronger than
        // escaping. Either way, no live <script> reaches the page.
        assert!(
            !html.contains("<script>"),
            "raw <script> neutralized: {html}"
        );
        assert!(!html.contains("</script>"), "no closing script tag: {html}");
        assert!(
            html.contains("<strong>bold</strong>"),
            "markdown still renders: {html}"
        );
    }

    #[test]
    fn render_comment_fragment_renders_relative_image_ref() {
        // A rewritten attachment ref is a relative `![](…)` and renders as
        // an <img> even with raw-HTML passthrough off.
        let html = render_comment_fragment("![chart](attachments/a_x-chart.png)");
        assert!(html.contains("<img"), "image tag: {html}");
        assert!(
            html.contains("attachments/a_x-chart.png"),
            "src preserved: {html}"
        );
    }
}
