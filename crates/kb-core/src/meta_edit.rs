//! In-place editors for the `kb-*` metadata an HTML artifact carries in
//! its source `<meta>` tags. Backs the `PATCH …/artifacts/{id}/meta`
//! route so tag/category edits made in the SPA write back into the
//! artifact's own source and re-index like any other file change.
//! (Markdown sources carry the same facets in frontmatter — see
//! [`crate::markdown::set_frontmatter_field`].)
//!
//! The editor is **byte-preserving outside the edited field**: it splices
//! a single attribute (or line) rather than reparse-and-reserialise, so an
//! edit produces a one-line diff and never disturbs scripts, comments,
//! whitespace, or the load-bearing `<template id="kb-prompt">`.
//!
//! INVARIANT (root CLAUDE.md): the scanner SKIPS `<script>`, `<style>`,
//! `<template>`, and `<!-- … -->` regions, and never scans inside another
//! tag's attribute values — any of those can legitimately contain literal
//! `<meta name="kb-tags" …>` *text* (the kb-prompt template especially).
//! Editing such text would corrupt the artifact. Only a real head `<meta>`
//! whose `name` matches is rewritten — the FIRST such tag, matching the
//! parser's `.next()` extraction semantics.

use std::ops::Range;

/// HTML-attribute escape, identical to `memory::render_artifact`'s private
/// `escape_attr`, so an edited meta line is byte-identical to a freshly
/// rendered one (clean round-trips).
fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Set the `kb-tags` meta to `slugged_tags` (already slugified by the
/// caller via [`crate::parser::slugify_tag`]). Joins with `", "` to match
/// the canonical render form. An empty slice REMOVES the meta (the indexer
/// then falls back to path-derived tags — there is no "no tags" form).
pub fn set_tags(html: &str, slugged_tags: &[String]) -> String {
    if slugged_tags.is_empty() {
        set_meta_content(html, "kb-tags", None)
    } else {
        set_meta_content(html, "kb-tags", Some(&slugged_tags.join(", ")))
    }
}

/// Set / insert / remove the `content` of the first head `<meta name="…">`
/// matching `name` (ASCII case-insensitive), preserving the rest of the
/// document byte-for-byte. `content == None` removes the tag (and its line
/// when the tag stood alone on it). `content` is escaped internally — pass
/// the raw value.
pub fn set_meta_content(html: &str, name: &str, content: Option<&str>) -> String {
    match find_meta(html, name) {
        Some(m) => match content {
            Some(c) => apply_content(html, &m, c),
            None => remove_tag_line(html, m.tag_start, m.tag_end),
        },
        None => match content {
            Some(c) => insert_new_meta(html, name, c),
            None => html.to_string(),
        },
    }
}

// --- internals ---------------------------------------------------------------

/// A matched head `<meta>` tag and the spans needed to edit its content.
struct MetaTag {
    tag_start: usize,
    /// Index just past the tag's terminating `>`.
    tag_end: usize,
    /// Inner value byte range of an existing `content` attr (excludes
    /// quotes); `None` when the tag has no `content` attr.
    content_inner: Option<Range<usize>>,
    /// Quote char of the existing `content` attr (`b'"'` / `b'\''`), or
    /// `None` for a bare (unquoted) value.
    content_quote: Option<u8>,
    /// Whole `content=…` token range (for normalising a non-double-quoted
    /// value).
    content_token: Option<Range<usize>>,
    /// Where to splice a new ` content="…"` when the tag has none — just
    /// before the closing `>` or `/>`.
    insert_content_at: usize,
}

/// One parsed attribute with absolute byte ranges into the source.
struct Attr {
    name_lc: String,
    value: Option<String>,
    value_inner: Option<Range<usize>>,
    quote: Option<u8>,
    token: Range<usize>,
}

fn find_meta(html: &str, name: &str) -> Option<MetaTag> {
    let b = html.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            if let Some(end) = skip_region_end(b, i) {
                i = end;
                continue;
            }
            if is_tag_open(b, i) {
                let te = find_tag_end(b, i);
                if tag_is(b, i, b"meta") {
                    if let Some(m) = parse_meta_tag(html, i, te, name) {
                        return Some(m);
                    }
                }
                i = te;
                continue;
            }
        }
        i += 1;
    }
    None
}

fn parse_meta_tag(html: &str, ts: usize, te: usize, target: &str) -> Option<MetaTag> {
    let attrs = parse_attrs(html, ts, te);
    let name_matches = attrs.iter().any(|a| {
        a.name_lc == "name"
            && a.value
                .as_deref()
                .map(|v| v.trim().eq_ignore_ascii_case(target))
                .unwrap_or(false)
    });
    if !name_matches {
        return None;
    }
    let content = attrs.iter().find(|a| a.name_lc == "content");
    Some(MetaTag {
        tag_start: ts,
        tag_end: te,
        content_inner: content.and_then(|a| a.value_inner.clone()),
        content_quote: content.and_then(|a| a.quote),
        content_token: content.map(|a| a.token.clone()),
        insert_content_at: insert_attr_pos(html.as_bytes(), ts, te),
    })
}

/// Parse the attributes of the tag at `html[ts..te]` into absolute spans.
fn parse_attrs(html: &str, ts: usize, te: usize) -> Vec<Attr> {
    let b = html.as_bytes();
    // Skip `<` and the tag name (alphanumeric run).
    let mut i = ts + 1;
    while i < te && b[i].is_ascii_alphanumeric() {
        i += 1;
    }
    let mut out = Vec::new();
    while i < te {
        while i < te && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= te || b[i] == b'>' {
            break;
        }
        if b[i] == b'/' {
            i += 1;
            continue;
        }
        let name_start = i;
        while i < te {
            let c = b[i];
            if c.is_ascii_whitespace() || c == b'=' || c == b'>' || c == b'/' {
                break;
            }
            i += 1;
        }
        let name = &html[name_start..i];
        while i < te && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = None;
        let mut value_inner = None;
        let mut quote = None;
        let mut token_end = i;
        if i < te && b[i] == b'=' {
            i += 1;
            while i < te && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < te && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vs = i;
                while i < te && b[i] != q {
                    i += 1;
                }
                let ve = i;
                if i < te {
                    i += 1; // past closing quote
                }
                value = Some(html[vs..ve].to_string());
                value_inner = Some(vs..ve);
                quote = Some(q);
                token_end = i;
            } else {
                let vs = i;
                while i < te {
                    let c = b[i];
                    if c.is_ascii_whitespace() || c == b'>' || c == b'/' {
                        break;
                    }
                    i += 1;
                }
                value = Some(html[vs..i].to_string());
                value_inner = Some(vs..i);
                token_end = i;
            }
        }
        out.push(Attr {
            name_lc: name.to_ascii_lowercase(),
            value,
            value_inner,
            quote,
            token: name_start..token_end,
        });
    }
    out
}

/// Position just before the tag's closing `>` (or the `/` of `/>`), where a
/// new ` content="…"` attribute is spliced.
fn insert_attr_pos(b: &[u8], ts: usize, te: usize) -> usize {
    let gt = te - 1; // the '>'
    let mut q = gt;
    while q > ts && b[q - 1].is_ascii_whitespace() {
        q -= 1;
    }
    if q > ts && b[q - 1] == b'/' {
        q - 1
    } else {
        gt
    }
}

fn apply_content(html: &str, m: &MetaTag, raw: &str) -> String {
    let esc = escape_attr(raw);
    if let Some(inner) = &m.content_inner {
        if m.content_quote == Some(b'"') {
            // Canonical case: splice just the value text — byte-preserving.
            return splice(html, inner.clone(), &esc);
        }
        // Single-quoted or bare: normalise the whole token to `content="…"`.
        let token = m
            .content_token
            .clone()
            .expect("content_inner implies token");
        return splice(html, token, &format!("content=\"{esc}\""));
    }
    // No content attr on the matched meta — insert one.
    let at = m.insert_content_at;
    let lead_space = at > 0 && !html.as_bytes()[at - 1].is_ascii_whitespace();
    let ins = if lead_space {
        format!(" content=\"{esc}\"")
    } else {
        format!("content=\"{esc}\"")
    };
    splice(html, at..at, &ins)
}

/// Replace `html[range]` with `repl`.
fn splice(html: &str, range: Range<usize>, repl: &str) -> String {
    let mut out = String::with_capacity(html.len() - (range.end - range.start) + repl.len());
    out.push_str(&html[..range.start]);
    out.push_str(repl);
    out.push_str(&html[range.end..]);
    out
}

/// Remove the tag `[start, end)`. If the tag stood alone on its line
/// (only whitespace around it), the whole line — including its trailing
/// newline — is removed so no blank line is left behind.
fn remove_tag_line(html: &str, start: usize, end: usize) -> String {
    let b = html.as_bytes();
    let mut ls = start;
    while ls > 0 && b[ls - 1] != b'\n' {
        ls -= 1;
    }
    let mut le = end;
    while le < b.len() && b[le] != b'\n' {
        le += 1;
    }
    if le < b.len() {
        le += 1; // include the newline
    }
    let alone = html[ls..start].trim().is_empty() && html[end..le].trim().is_empty();
    if alone {
        splice(html, ls..le, "")
    } else {
        splice(html, start..end, "")
    }
}

fn insert_new_meta(html: &str, name: &str, raw: &str) -> String {
    let esc = escape_attr(raw);
    let meta = format!("<meta name=\"{name}\" content=\"{esc}\">");
    let b = html.as_bytes();
    let content_start = if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };

    // Single pass over the head, recording candidate anchors by priority.
    let mut after_title = None;
    let mut after_charset = None;
    let mut after_head = None;
    let mut after_html = None;
    let mut i = content_start;
    while i < b.len() {
        if b[i] == b'<' {
            if let Some(end) = skip_region_end(b, i) {
                i = end;
                continue;
            }
            if is_tag_open(b, i) {
                let te = find_tag_end(b, i);
                if tag_is(b, i, b"body") || close_tag_is(b, i, b"head") {
                    break; // past the head — stop looking
                }
                if close_tag_is(b, i, b"title") {
                    after_title = Some(te);
                    break; // highest priority; nothing better follows
                }
                if after_charset.is_none() && tag_is(b, i, b"meta") && meta_has_charset(html, i, te)
                {
                    after_charset = Some(te);
                }
                if after_head.is_none() && tag_is(b, i, b"head") {
                    after_head = Some(te);
                }
                if after_html.is_none() && tag_is(b, i, b"html") {
                    after_html = Some(te);
                }
                i = te;
                continue;
            }
        }
        i += 1;
    }

    match after_title.or(after_charset).or(after_head).or(after_html) {
        Some(at) => format!("{}\n{}{}", &html[..at], meta, &html[at..]),
        None => format!(
            "{}{}\n{}",
            &html[..content_start],
            meta,
            &html[content_start..]
        ),
    }
}

fn meta_has_charset(html: &str, ts: usize, te: usize) -> bool {
    parse_attrs(html, ts, te)
        .iter()
        .any(|a| a.name_lc == "charset")
}

/// If `b[i..]` opens a skip region (`<!-- -->`, `<script>`, `<style>`,
/// `<template>`), return the index just past its close.
fn skip_region_end(b: &[u8], i: usize) -> Option<usize> {
    if b[i..].starts_with(b"<!--") {
        return Some(find_sub(b, i + 4, b"-->").map(|p| p + 3).unwrap_or(b.len()));
    }
    for (open, close) in [
        (&b"script"[..], &b"</script"[..]),
        (&b"style"[..], &b"</style"[..]),
        (&b"template"[..], &b"</template"[..]),
    ] {
        if tag_is(b, i, open) {
            let open_end = find_tag_end(b, i);
            return Some(match find_sub_ci(b, open_end, close) {
                Some(p) => find_tag_end(b, p),
                None => b.len(),
            });
        }
    }
    None
}

/// `b[i]=='<'` then `name` (ASCII case-insensitive) then a tag-name
/// boundary (whitespace / `>` / `/`).
fn tag_is(b: &[u8], i: usize, name_lc: &[u8]) -> bool {
    if b.get(i) != Some(&b'<') {
        return false;
    }
    let s = i + 1;
    if s + name_lc.len() > b.len() || !b[s..s + name_lc.len()].eq_ignore_ascii_case(name_lc) {
        return false;
    }
    match b.get(s + name_lc.len()) {
        None => true,
        Some(&c) => c.is_ascii_whitespace() || c == b'>' || c == b'/',
    }
}

/// `b[i..]` is `</name` (ASCII case-insensitive) then boundary.
fn close_tag_is(b: &[u8], i: usize, name_lc: &[u8]) -> bool {
    if b.get(i) != Some(&b'<') || b.get(i + 1) != Some(&b'/') {
        return false;
    }
    let s = i + 2;
    if s + name_lc.len() > b.len() || !b[s..s + name_lc.len()].eq_ignore_ascii_case(name_lc) {
        return false;
    }
    match b.get(s + name_lc.len()) {
        None => true,
        Some(&c) => c.is_ascii_whitespace() || c == b'>',
    }
}

fn is_tag_open(b: &[u8], i: usize) -> bool {
    if b.get(i) != Some(&b'<') {
        return false;
    }
    match b.get(i + 1) {
        Some(&c) if c.is_ascii_alphabetic() || c == b'!' || c == b'?' => true,
        Some(&b'/') => b.get(i + 2).is_some_and(|c| c.is_ascii_alphabetic()),
        _ => false,
    }
}

/// Index just past the `>` that ends the tag at `start`, treating `>` inside
/// quoted attribute values as ordinary chars.
fn find_tag_end(b: &[u8], start: usize) -> usize {
    let mut j = start + 1;
    let mut quote = 0u8;
    while j < b.len() {
        let c = b[j];
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'>' {
            return j + 1;
        }
        j += 1;
    }
    b.len()
}

fn find_sub(hay: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    (from..=hay.len().saturating_sub(needle.len())).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn find_sub_ci(hay: &[u8], from: usize, needle_lc: &[u8]) -> Option<usize> {
    if needle_lc.is_empty() || from >= hay.len() {
        return None;
    }
    (from..=hay.len().saturating_sub(needle_lc.len()))
        .find(|&i| hay[i..i + needle_lc.len()].eq_ignore_ascii_case(needle_lc))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(html: &str) -> Vec<String> {
        crate::parser::extract(html).tags
    }

    #[test]
    fn replace_existing_double_quoted() {
        let html = "<head><title>t</title>\n<meta name=\"kb-tags\" content=\"a, b\">\n</head>";
        let out = set_tags(html, &["c".into(), "d".into()]);
        assert!(out.contains("content=\"c, d\""));
        assert!(!out.contains("a, b"));
        // Everything outside the value is byte-identical.
        assert_eq!(out, html.replace("a, b", "c, d"));
        assert_eq!(tags(&out), vec!["c", "d"]);
    }

    #[test]
    fn replace_attr_order_swapped() {
        let html = "<meta content=\"old\" name=\"kb-tags\">";
        let out = set_tags(html, &["new".into()]);
        assert_eq!(out, "<meta content=\"new\" name=\"kb-tags\">");
        assert_eq!(tags(&out), vec!["new"]);
    }

    #[test]
    fn replace_single_quoted_normalises_to_double() {
        let html = "<meta name='kb-tags' content='old'>";
        let out = set_tags(html, &["new".into()]);
        assert_eq!(out, "<meta name='kb-tags' content=\"new\">");
        assert_eq!(tags(&out), vec!["new"]);
    }

    #[test]
    fn replace_unquoted_normalises() {
        let html = "<meta name=kb-tags content=old>";
        let out = set_tags(html, &["new".into()]);
        assert_eq!(out, "<meta name=kb-tags content=\"new\">");
    }

    #[test]
    fn replace_case_insensitive_name() {
        let html = "<META NAME=\"KB-TAGS\" CONTENT=\"x\">";
        let out = set_tags(html, &["y".into()]);
        assert!(out.contains("CONTENT=\"y\""));
    }

    #[test]
    fn insert_content_when_attr_absent() {
        let html = "<meta name=\"kb-tags\">";
        let out = set_tags(html, &["a".into()]);
        assert_eq!(out, "<meta name=\"kb-tags\" content=\"a\">");
    }

    #[test]
    fn insert_content_self_closing() {
        let html = "<meta name=\"kb-tags\" />";
        let out = set_tags(html, &["a".into()]);
        assert_eq!(out, "<meta name=\"kb-tags\" content=\"a\"/>");
        assert_eq!(tags(&out), vec!["a"]);
    }

    #[test]
    fn gt_inside_value_does_not_truncate() {
        let html = "<meta name=\"kb-tags\" content=\"a > b\"><div>after</div>";
        let out = set_tags(html, &["x".into()]);
        assert_eq!(out, "<meta name=\"kb-tags\" content=\"x\"><div>after</div>");
    }

    #[test]
    fn insert_after_title_when_absent() {
        let html =
            "<html><head><meta charset=\"utf-8\">\n<title>T</title>\n</head><body>x</body></html>";
        let out = set_tags(html, &["a".into(), "b".into()]);
        let expect = "<html><head><meta charset=\"utf-8\">\n<title>T</title>\n<meta name=\"kb-tags\" content=\"a, b\">\n</head><body>x</body></html>";
        assert_eq!(out, expect);
        assert_eq!(tags(&out), vec!["a", "b"]);
    }

    #[test]
    fn insert_after_charset_when_no_title() {
        let html = "<head><meta charset=\"utf-8\"></head>";
        let out = set_meta_content(html, "kb-category", Some("research"));
        assert_eq!(
            out,
            "<head><meta charset=\"utf-8\">\n<meta name=\"kb-category\" content=\"research\"></head>"
        );
    }

    #[test]
    fn insert_when_no_head_prepends() {
        let html = "<p>just a fragment</p>";
        let out = set_tags(html, &["a".into()]);
        assert_eq!(
            out,
            "<meta name=\"kb-tags\" content=\"a\">\n<p>just a fragment</p>"
        );
    }

    #[test]
    fn remove_when_empty_drops_lone_line() {
        let html = "<head>\n<meta name=\"kb-tags\" content=\"a\">\n<title>t</title>\n</head>";
        let out = set_tags(html, &[]);
        assert_eq!(out, "<head>\n<title>t</title>\n</head>");
    }

    #[test]
    fn remove_handles_crlf() {
        let html = "<head>\r\n<meta name=\"kb-tags\" content=\"a\">\r\n<title>t</title>\r\n</head>";
        let out = set_tags(html, &[]);
        assert_eq!(out, "<head>\r\n<title>t</title>\r\n</head>");
    }

    #[test]
    fn remove_inline_meta_keeps_surrounding() {
        let html = "<head><title>t</title><meta name=\"kb-tags\" content=\"a\"></head>";
        let out = set_tags(html, &[]);
        assert_eq!(out, "<head><title>t</title></head>");
    }

    #[test]
    fn multiple_metas_only_first_rewritten() {
        let html =
            "<meta name=\"kb-tags\" content=\"first\">\n<meta name=\"kb-tags\" content=\"second\">";
        let out = set_tags(html, &["x".into()]);
        assert_eq!(
            out,
            "<meta name=\"kb-tags\" content=\"x\">\n<meta name=\"kb-tags\" content=\"second\">"
        );
    }

    // invariant:12 skip-template
    #[test]
    fn kb_prompt_template_untouched() {
        // A literal kb-tags meta INSIDE the prompt template is data, not a
        // head meta — it must be preserved verbatim; the real head meta is
        // the one edited.
        let html = "<head>\n<title>t</title>\n<meta name=\"kb-tags\" content=\"real\">\n\
                    <template id=\"kb-prompt\">\n\
                    Use <meta name=\"kb-tags\" content=\"example, in, prompt\"> like this.\n\
                    </template>\n</head>";
        let out = set_tags(html, &["edited".into()]);
        assert!(out.contains("content=\"edited\""));
        // The template's literal meta is preserved exactly.
        assert!(
            out.contains("Use <meta name=\"kb-tags\" content=\"example, in, prompt\"> like this.")
        );
        // Only one real replacement happened.
        assert_eq!(out.matches("content=\"edited\"").count(), 1);
    }

    // invariant:12 skip-template
    #[test]
    fn script_and_comment_meta_untouched() {
        let html = "<head>\n<title>t</title>\n\
                    <script>var s = '<meta name=\"kb-tags\" content=\"in-script\">';</script>\n\
                    <!-- <meta name=\"kb-tags\" content=\"in-comment\"> -->\n\
                    <meta name=\"kb-tags\" content=\"real\">\n</head>";
        let out = set_tags(html, &["edited".into()]);
        assert!(out.contains("in-script"));
        assert!(out.contains("in-comment"));
        assert!(out.contains("content=\"edited\""));
        assert!(!out.contains("\"real\""));
    }

    #[test]
    fn meta_inside_attribute_value_untouched() {
        // A `<meta …>` lurking inside another tag's quoted attribute must
        // not be mistaken for a real tag.
        let html = "<div data-x=\"<meta name='kb-tags' content='trap'>\"></div>\n<meta name=\"kb-tags\" content=\"real\">";
        let out = set_tags(html, &["edited".into()]);
        assert!(out.contains("content='trap'"));
        assert!(out.contains("content=\"edited\""));
        assert!(!out.contains("\"real\""));
    }

    #[test]
    fn bom_preserved_on_prepend() {
        let html = "\u{feff}<p>x</p>";
        let out = set_tags(html, &["a".into()]);
        assert_eq!(
            out,
            "\u{feff}<meta name=\"kb-tags\" content=\"a\">\n<p>x</p>"
        );
    }

    #[test]
    fn escapes_special_chars() {
        let html = "<meta name=\"kb-category\" content=\"x\">";
        let out = set_meta_content(html, "kb-category", Some("a&b\"c<d>"));
        assert!(out.contains("content=\"a&amp;b&quot;c&lt;d&gt;\""));
    }

    #[test]
    fn roundtrip_matches_render_artifact() {
        let rendered = crate::memory::render_artifact(
            "Title",
            "<p>body</p>",
            "notes",
            &["a".to_string(), "b".to_string()],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        let edited = set_tags(&rendered, &["c".to_string(), "d".to_string()]);
        let direct = crate::memory::render_artifact(
            "Title",
            "<p>body</p>",
            "notes",
            &["c".to_string(), "d".to_string()],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(edited, direct);
    }

    #[test]
    fn no_op_remove_when_absent() {
        let html = "<head><title>t</title></head>";
        assert_eq!(set_tags(html, &[]), html);
    }
}
