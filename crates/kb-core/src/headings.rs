//! Heading anchors — the Rust twin of the iframe runtime's slugifier.
//!
//! The artifact iframe runtime (`runtime_js` in
//! `kb-server/src/routes/artifact.rs`) walks `h1, h2, h3` on load, gives every
//! heading that lacks one an `id` of the form `kb-h-<slug>`, and posts a
//! `kb:toc` to the parent. `kb:scroll-to-id` then scrolls the iframe to any of
//! those ids. That makes the heading id the ONE addressable position inside a
//! served artifact — but it only exists in the browser, computed from the
//! rendered DOM.
//!
//! This module recomputes the same ids **server-side, from the raw source**,
//! and pairs each with the SOURCE LINE the heading sits on. That is what lets
//! a file position (a session-replay beat's `line_range`, a diff hunk, a
//! comment anchor) become "scroll the iframe to `kb-h-the-storage-actor`" via
//! [`slug_for_line`].
//!
//! ### Lock-step with the JS (load-bearing)
//!
//! [`slugify`] and [`heading_id`] replicate the runtime's rules EXACTLY:
//!
//! ```js
//! const slug = text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
//! h.id = 'kb-h-' + (slug || ('h' + out.length));
//! ```
//!
//! Three consequences that are easy to get wrong and are pinned in the tests:
//!
//! * The JS **does not de-duplicate**. Two `## Setup` headings both become
//!   `kb-h-setup`; the browser resolves `#kb-h-setup` to the first. We match
//!   that rather than inventing a `-2` suffix the DOM would never carry.
//! * The empty-slug fallback is `h<ordinal>` where the ordinal is the index of
//!   the heading **among emitted headings** (blank-text headings are skipped
//!   before the counter advances) — not the raw element index.
//! * `.replace(/(^-|-$)/g, '')` strips at most ONE leading and ONE trailing
//!   dash (`^` can only match at index 0 without the `m` flag). Runs of
//!   non-alphanumerics have already collapsed to a single `-`, so this is the
//!   same as a full trim — but it is replicated literally.
//!
//! `heading_anchors_test::rust_and_js_slugifiers_agree` pins the JS source
//! text itself, so changing the runtime's regex fails a kb-core test.
//!
//! ### Scope
//!
//! HTML only. A Markdown note's `## Heading` gets its `id` from the *rendered*
//! HTML, which this module deliberately does not reproduce (that would mean
//! re-implementing comrak's render); [`heading_anchors`] simply returns no
//! anchors for a Markdown source. Callers that need note anchors should render
//! first, or use the existing `crate::anchors` Chapter machinery.

/// The id prefix the iframe runtime stamps on every heading it names.
pub const HEADING_ID_PREFIX: &str = "kb-h-";

/// One `(source line, element id)` pair. The line is 1-based and points at
/// the line the `<hN` tag opens on; the id is what `kb:scroll-to-id` takes.
pub type HeadingAnchor = (u32, String);

/// The runtime's slug rule: lowercase, every run of non-`[a-z0-9]` to a single
/// `-`, then strip one leading and one trailing `-`. May return `""` (a
/// heading with no ASCII alphanumerics at all) — [`heading_id`] owns the
/// fallback.
pub fn slugify(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut in_run = false;
    for ch in lower.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    // `.replace(/(^-|-$)/g, '')` — exactly one leading, exactly one trailing.
    let s = out.strip_prefix('-').unwrap_or(&out);
    let s = s.strip_suffix('-').unwrap_or(s);
    s.to_string()
}

/// The full DOM id the runtime assigns: `kb-h-<slug>`, falling back to
/// `kb-h-h<ordinal>` when the heading text has no sluggable characters.
/// `ordinal` is the heading's index among EMITTED headings.
pub fn heading_id(text: &str, ordinal: usize) -> String {
    let slug = slugify(text);
    if slug.is_empty() {
        format!("{HEADING_ID_PREFIX}h{ordinal}")
    } else {
        format!("{HEADING_ID_PREFIX}{slug}")
    }
}

/// Scan an artifact's raw HTML for `h1`/`h2`/`h3` headings and return
/// `(line, id)` in document order.
///
/// Mirrors the runtime's DOM view, not a naive regex sweep:
///
/// * only `h1`, `h2`, `h3` (the runtime's `querySelectorAll('h1, h2, h3')`);
/// * heading text is `textContent` — nested tags contribute their text with
///   NO separator inserted (`a<em>b</em>` is `ab`, not `a b`), comments
///   contribute nothing, and the basic HTML entities are decoded;
/// * a heading whose trimmed text is empty is skipped entirely (it also does
///   not consume an ordinal);
/// * an existing `id` attribute wins verbatim — the runtime only assigns one
///   when `!h.id`;
/// * `<script>` / `<style>` / `<template>` subtrees are skipped. Script and
///   style contents are TEXT (an `<h1>` there is not an element), and a
///   `<template>`'s content is an inert fragment that `querySelectorAll` on
///   the document never sees — which is exactly what keeps the kb-prompt
///   bundle (invariant #5) out of the anchor list.
pub fn heading_anchors(raw_source: &str) -> Vec<HeadingAnchor> {
    let src = raw_source;
    let bytes = src.as_bytes();
    let mut out: Vec<HeadingAnchor> = Vec::new();
    // Monotonic line counter: every scan position only ever moves forward, so
    // the line number is maintained incrementally instead of re-counting
    // newlines from the start for each heading.
    let mut line: u32 = 1;
    let mut counted_to: usize = 0;
    let mut i: usize = 0;

    while i < bytes.len() {
        let Some(lt) = memchr(bytes, b'<', i) else {
            break;
        };
        // Advance the line counter to the `<`.
        line += count_newlines(&bytes[counted_to..lt]);
        counted_to = lt;

        // Comment.
        if src[lt..].starts_with("<!--") {
            i = match src[lt + 4..].find("-->") {
                Some(off) => lt + 4 + off + 3,
                None => bytes.len(),
            };
            continue;
        }

        let Some((name, closing)) = tag_name_at(src, lt) else {
            i = lt + 1;
            continue;
        };

        if !closing && matches!(name.as_str(), "script" | "style" | "template") {
            let Some(open_end) = tag_end(src, lt) else {
                break;
            };
            if src[..open_end].ends_with("/>") {
                i = open_end;
                continue;
            }
            i = skip_element(src, open_end, &name);
            continue;
        }

        if !closing && matches!(name.as_str(), "h1" | "h2" | "h3") {
            let Some(open_end) = tag_end(src, lt) else {
                break;
            };
            let attrs = &src[lt + 1 + name.len()..open_end.saturating_sub(1)];
            // A self-closing `<h2/>` has no text content — skipped like any
            // empty heading.
            if !attrs.trim_end().ends_with('/') {
                let close = find_close_tag(src, open_end, &name).unwrap_or(bytes.len());
                let text = text_content(&src[open_end..close]);
                let text = text.trim();
                if !text.is_empty() {
                    let id = match attr_value(attrs, "id") {
                        Some(existing) if !existing.trim().is_empty() => {
                            existing.trim().to_string()
                        }
                        _ => heading_id(text, out.len()),
                    };
                    out.push((line, id));
                }
            }
            i = open_end;
            continue;
        }

        i = lt + 1;
    }

    out
}

/// The id of the nearest heading at or ABOVE `line` — "the agent was editing
/// around here" → "scroll the iframe to this section". `None` when `line`
/// sits before the first heading (a preamble) or there are no headings.
///
/// `anchors` is expected in document order, as [`heading_anchors`] returns it.
pub fn slug_for_line(anchors: &[HeadingAnchor], line: u32) -> Option<&str> {
    anchors
        .iter()
        .rev()
        .find(|(l, _)| *l <= line)
        .map(|(_, id)| id.as_str())
}

// --- tiny HTML scanning helpers ---------------------------------------------

fn memchr(hay: &[u8], needle: u8, from: usize) -> Option<usize> {
    hay[from..]
        .iter()
        .position(|b| *b == needle)
        .map(|p| p + from)
}

fn count_newlines(bytes: &[u8]) -> u32 {
    bytes.iter().filter(|b| **b == b'\n').count() as u32
}

/// Lowercase tag name at `lt` (which must index a `<`), plus whether it is a
/// closing tag. `None` when `<` doesn't open a tag.
fn tag_name_at(src: &str, lt: usize) -> Option<(String, bool)> {
    let rest = &src[lt + 1..];
    let (rest, closing) = match rest.strip_prefix('/') {
        Some(r) => (r, true),
        None => (rest, false),
    };
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if name.is_empty() {
        return None;
    }
    Some((name.to_ascii_lowercase(), closing))
}

/// Byte index just PAST the `>` that ends the tag starting at `lt`, quoted
/// attribute values respected.
fn tag_end(src: &str, lt: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut i = lt + 1;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let b = bytes[i];
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i + 1),
            None => {}
        }
        i += 1;
    }
    None
}

/// Value of the named attribute in a tag's attribute run (the text between
/// the tag name and its `>`), entity-decoded. A real attribute walk, not a
/// substring search — `data-idx="3"` must not answer a query for `id`.
/// Handles double-quoted, single-quoted and bare values.
fn attr_value(attrs: &str, want: &str) -> Option<String> {
    let b = attrs.as_bytes();
    let mut i = 0usize;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' {
            i += 1;
        }
        if i == start {
            i += 1;
            continue;
        }
        let name = &attrs[start..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value: Option<&str> = None;
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let quote = b[i];
                i += 1;
                let vs = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                value = Some(&attrs[vs..i]);
                if i < b.len() {
                    i += 1;
                }
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = Some(&attrs[vs..i]);
            }
        }
        if name.eq_ignore_ascii_case(want) {
            return value.map(decode_entities);
        }
    }
    None
}

/// Index just past `</name>` starting the search at `from`; `src.len()` when
/// the document is truncated. `template` nests, so its depth is tracked.
fn skip_element(src: &str, from: usize, name: &str) -> usize {
    let mut depth = 1usize;
    let mut i = from;
    let nestable = name == "template";
    while i < src.len() {
        let Some(lt) = memchr(src.as_bytes(), b'<', i) else {
            return src.len();
        };
        match tag_name_at(src, lt) {
            Some((n, closing)) if n == name => {
                let end = tag_end(src, lt).unwrap_or(src.len());
                if closing {
                    depth -= 1;
                    if depth == 0 {
                        return end;
                    }
                } else if nestable && !src[..end].ends_with("/>") {
                    depth += 1;
                }
                i = end;
            }
            _ => i = lt + 1,
        }
    }
    src.len()
}

/// Index of the `<` of the matching `</name>` after `from`, or `None`.
fn find_close_tag(src: &str, from: usize, name: &str) -> Option<usize> {
    let mut i = from;
    while i < src.len() {
        let lt = memchr(src.as_bytes(), b'<', i)?;
        match tag_name_at(src, lt) {
            Some((n, true)) if n == name => return Some(lt),
            _ => i = lt + 1,
        }
    }
    None
}

/// DOM `textContent` of an HTML fragment: tags and comments removed with NO
/// separator inserted, entities decoded, whitespace otherwise untouched.
fn text_content(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    let mut i = 0usize;
    while i < fragment.len() {
        match memchr(fragment.as_bytes(), b'<', i) {
            None => {
                out.push_str(&fragment[i..]);
                break;
            }
            Some(lt) => {
                out.push_str(&fragment[i..lt]);
                if fragment[lt..].starts_with("<!--") {
                    i = match fragment[lt + 4..].find("-->") {
                        Some(off) => lt + 4 + off + 3,
                        None => fragment.len(),
                    };
                } else if tag_name_at(fragment, lt).is_some() || fragment[lt..].starts_with("<!") {
                    i = tag_end(fragment, lt).unwrap_or(fragment.len());
                } else {
                    // A bare `<` in text (`a < b`) is not a tag.
                    out.push('<');
                    i = lt + 1;
                }
            }
        }
    }
    decode_entities(&out)
}

/// The handful of entities that actually show up in heading text. Anything
/// else is left verbatim — it would slug to `-` either way.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        // Byte-wise so a multibyte char can never be sliced in half.
        let semi = tail.as_bytes().iter().take(12).position(|b| *b == b';');
        let Some(semi) = semi else {
            out.push('&');
            rest = &tail[1..];
            continue;
        };
        let ent = &tail[1..semi];
        let decoded = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16)
                .ok()
                .and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &tail[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tricky-case fixture set. Each entry is
    /// `(heading text, ordinal, expected id)`. Every expectation below was
    /// produced by RUNNING the runtime's own two `.replace()` calls in node
    /// over the same inputs (2026-07-26) — not derived by eye — and the JS
    /// source text it was taken from is pinned by
    /// `rust_and_js_slugifiers_agree` so a change there fails this crate.
    const SLUG_CASES: &[(&str, usize, &str)] = &[
        ("Hello, World!", 0, "kb-h-hello-world"),
        ("  Spaced   Out  ", 0, "kb-h-spaced-out"),
        ("Café Society", 0, "kb-h-caf-society"),
        ("C++ & Rust", 0, "kb-h-c-rust"),
        ("2026: The Year", 0, "kb-h-2026-the-year"),
        ("--dashes--", 0, "kb-h-dashes"),
        ("...", 0, "kb-h-h0"),
        // No ASCII alphanumerics at all → the ordinal fallback, and the
        // ordinal is the position among EMITTED headings.
        ("日本語", 3, "kb-h-h3"),
        ("", 7, "kb-h-h7"),
        ("§ ¶ †", 2, "kb-h-h2"),
        ("MiXeD CaSe", 0, "kb-h-mixed-case"),
        ("trailing punctuation!!!", 0, "kb-h-trailing-punctuation"),
        ("under_scores and-dashes", 0, "kb-h-under-scores-and-dashes"),
    ];

    #[test]
    fn slugify_matches_the_pinned_cases() {
        for (text, ordinal, want) in SLUG_CASES {
            assert_eq!(&heading_id(text, *ordinal), want, "text={text:?}");
        }
    }

    #[test]
    fn slugify_handles_a_very_long_heading_without_truncation() {
        // The runtime truncates the TOC *label* at 60 chars; the id is never
        // truncated. Don't "helpfully" cap it here.
        let text = "a ".repeat(200);
        let id = heading_id(&text, 0);
        assert_eq!(id.len(), HEADING_ID_PREFIX.len() + 399);
        assert!(id.starts_with("kb-h-a-a-a"));
        assert!(id.ends_with('a'));
    }

    // Lock-step guard: the Rust rules above are a transcription of the JS in
    // kb-server's `runtime_js`. If someone edits that slugifier, this fails.
    #[test]
    fn rust_and_js_slugifiers_agree() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../kb-server/src/routes/artifact.rs");
        let Ok(js) = std::fs::read_to_string(&path) else {
            // Packaged crate / partial checkout: nothing to compare against.
            eprintln!("skipping: {} not readable", path.display());
            return;
        };
        // The exact source lines this module transcribes.
        for needle in [
            "document.querySelectorAll('h1, h2, h3')",
            "const slug = text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');",
            "h.id = 'kb-h-' + (slug || ('h' + out.length));",
        ] {
            assert!(
                js.contains(needle),
                "the iframe runtime's slugifier changed — kb_core::headings must be updated in \
                 lock-step. Missing from {}:\n  {needle}",
                path.display()
            );
        }
        // …and it still must NOT de-duplicate (no id-uniquing pass snuck in).
        assert!(
            !js.contains("kb-h-' + slug + '-"),
            "the runtime started de-duplicating heading ids; slugify() must follow"
        );
    }

    #[test]
    fn repeated_headings_are_not_de_duplicated() {
        let html = "<h2>Setup</h2>\n<h2>Setup</h2>\n<h3>Setup</h3>";
        let got = heading_anchors(html);
        assert_eq!(
            got,
            vec![
                (1, "kb-h-setup".to_string()),
                (2, "kb-h-setup".to_string()),
                (3, "kb-h-setup".to_string()),
            ]
        );
    }

    #[test]
    fn anchors_carry_one_based_source_lines_in_document_order() {
        let html = concat!(
            "<html>\n",
            "<body>\n",
            "<h1>The Title</h1>\n",
            "<p>prose</p>\n",
            "\n",
            "<h2>First Section</h2>\n",
            "<p>more</p>\n",
            "<h3>A Detail</h3>\n",
            "</body>\n",
        );
        assert_eq!(
            heading_anchors(html),
            vec![
                (3, "kb-h-the-title".to_string()),
                (6, "kb-h-first-section".to_string()),
                (8, "kb-h-a-detail".to_string()),
            ]
        );
    }

    #[test]
    fn only_h1_h2_h3_count() {
        let html = "<h1>One</h1><h4>Four</h4><h5>Five</h5><h6>Six</h6><h3>Three</h3>";
        let ids: Vec<_> = heading_anchors(html)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(ids, vec!["kb-h-one", "kb-h-three"]);
    }

    #[test]
    fn nested_markup_is_text_content_with_no_separator() {
        // textContent inserts NOTHING between nodes.
        let html = "<h2>The <em>fast</em> path</h2><h2>a<em>b</em>c</h2>";
        let ids: Vec<_> = heading_anchors(html)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(ids, vec!["kb-h-the-fast-path", "kb-h-abc"]);
    }

    #[test]
    fn entities_are_decoded_before_slugging() {
        let html = "<h2>Tom &amp; Jerry</h2><h2>&lt;pre&gt; blocks</h2><h2>caf&#233; time</h2>";
        let ids: Vec<_> = heading_anchors(html)
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(
            ids,
            vec!["kb-h-tom-jerry", "kb-h-pre-blocks", "kb-h-caf-time"]
        );
    }

    #[test]
    fn blank_headings_are_skipped_and_do_not_consume_an_ordinal() {
        // `if (!text) continue;` runs BEFORE `out.push`, so the empty h2
        // never advances `out.length`.
        let html = "<h2></h2>\n<h2>   </h2>\n<h2>日本語</h2>\n<h2>Real</h2>";
        assert_eq!(
            heading_anchors(html),
            vec![(3, "kb-h-h0".to_string()), (4, "kb-h-real".to_string())]
        );
    }

    #[test]
    fn existing_id_attribute_wins_verbatim() {
        let html = concat!(
            "<h2 id=\"design-notes\">Design Notes</h2>\n",
            "<h2 id='single-quoted'>Other</h2>\n",
            "<h2 class=\"x\" id=custom-bare>Bare</h2>\n",
            "<h2 id=\"\">Empty id falls back</h2>\n",
        );
        assert_eq!(
            heading_anchors(html),
            vec![
                (1, "design-notes".to_string()),
                (2, "single-quoted".to_string()),
                (3, "custom-bare".to_string()),
                (4, "kb-h-empty-id-falls-back".to_string()),
            ]
        );
    }

    #[test]
    fn attributes_that_merely_contain_id_are_not_the_id() {
        let html = "<h2 data-idx=\"3\" aria-hidden=\"false\">Real Heading</h2>";
        assert_eq!(heading_anchors(html), vec![(1, "kb-h-real-heading".into())]);
    }

    // invariant:5 — the kb-prompt bundle must never surface as an anchor.
    #[test]
    fn script_style_and_template_subtrees_are_skipped() {
        let html = concat!(
            "<h1>Real</h1>\n",
            "<script>\n",
            "  var s = '<h2>Not A Heading</h2>';\n",
            "</script>\n",
            "<style>\n",
            "  h2::before { content: '<h2>nope</h2>'; }\n",
            "</style>\n",
            "<template id=\"kb-prompt\">\n",
            "  <h2>Prompt Heading</h2>\n",
            "  <template><h3>Nested</h3></template>\n",
            "</template>\n",
            "<h2>After</h2>\n",
        );
        assert_eq!(
            heading_anchors(html),
            vec![(1, "kb-h-real".to_string()), (12, "kb-h-after".to_string())]
        );
    }

    #[test]
    fn comments_contribute_nothing() {
        let html = "<!-- <h1>ghost</h1> -->\n<h2>Vis<!-- mid -->ible</h2>";
        assert_eq!(heading_anchors(html), vec![(2, "kb-h-visible".into())]);
    }

    #[test]
    fn malformed_html_does_not_panic_or_hang() {
        for html in [
            "",
            "<",
            "<h2",
            "<h2>unclosed",
            "<h2 id=\"x>still unclosed",
            "<script>never closed <h2>x</h2>",
            "<template><h2>x</h2>",
            "<h2>a < b</h2>",
            "<h2/>",
            "&amp",
            "<h2>&#xzz; &#99999999999;</h2>",
        ] {
            let _ = heading_anchors(html);
        }
        // A `<` in prose is text, not a tag.
        assert_eq!(
            heading_anchors("<h2>a < b</h2>"),
            vec![(1, "kb-h-a-b".into())]
        );
        // A truncated document still yields what it can.
        assert_eq!(
            heading_anchors("<h2>unclosed"),
            vec![(1, "kb-h-unclosed".into())]
        );
    }

    #[test]
    fn multibyte_source_lines_are_counted_correctly() {
        let html = "<p>ünïcödé prose 日本語</p>\n<h2>Résumé Section</h2>";
        assert_eq!(
            heading_anchors(html),
            vec![(2, "kb-h-r-sum-section".into())]
        );
    }

    #[test]
    fn slug_for_line_finds_the_nearest_preceding_heading() {
        let anchors = heading_anchors(concat!(
            "<p>preamble</p>\n", // 1
            "<h1>Top</h1>\n",    // 2
            "<p>x</p>\n",        // 3
            "<h2>Middle</h2>\n", // 4
            "<p>y</p>\n",        // 5
            "<h2>End</h2>\n",    // 6
        ));
        assert_eq!(slug_for_line(&anchors, 1), None, "before the first heading");
        assert_eq!(slug_for_line(&anchors, 2), Some("kb-h-top"));
        assert_eq!(slug_for_line(&anchors, 3), Some("kb-h-top"));
        assert_eq!(slug_for_line(&anchors, 4), Some("kb-h-middle"));
        assert_eq!(slug_for_line(&anchors, 5), Some("kb-h-middle"));
        assert_eq!(slug_for_line(&anchors, 99), Some("kb-h-end"));
        assert_eq!(slug_for_line(&[], 5), None);
    }

    #[test]
    fn markdown_source_yields_no_anchors() {
        // Documented scope limit: the ids live in the RENDERED html.
        assert!(heading_anchors("# Title\n\n## Section\n").is_empty());
    }

    #[test]
    fn heading_anchors_is_deterministic() {
        let html = "<h1>A</h1><h2>B</h2><h3>C</h3>";
        assert_eq!(heading_anchors(html), heading_anchors(html));
    }
}
