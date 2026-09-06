//! Integration coverage for first-class Markdown ingestion: the public
//! ingest gate (`indexer::is_indexable` / `is_markdown`) and
//! `parser::extract_markdown` (render → extract → frontmatter override).

use kb_core::indexer::{is_indexable, is_markdown};
use kb_core::parser::extract_markdown;
use std::path::Path;

#[test]
fn gate_recognises_md_and_html() {
    assert!(is_indexable(Path::new("a.md")));
    assert!(is_indexable(Path::new("a.markdown")));
    assert!(is_indexable(Path::new("a.HTML")));
    assert!(is_indexable(Path::new("a.htm")));
    assert!(!is_indexable(Path::new("a.txt")));
    assert!(!is_indexable(Path::new("a")));
    assert!(is_markdown(Path::new("a.MD")));
    assert!(!is_markdown(Path::new("a.html")));
}

#[test]
fn extract_markdown_pulls_frontmatter_body_and_headings() {
    let src = "---\n\
title: Spec\n\
kb-category: design\n\
kb-tags: Rust, Async Errors\n\
kb-salience: 0.7\n\
---\n\
# Heading\n\n\
Some **body** text with a [link](./other.md).\n\n\
## Sub\n\n\
- [x] done\n";
    let (f, html) = extract_markdown(src);
    assert_eq!(f.title.as_deref(), Some("Spec"));
    assert_eq!(f.kb_category.as_deref(), Some("design"));
    // Tags are slugified exactly like the HTML `<meta name="kb-tags">` path.
    assert_eq!(f.tags, vec!["rust", "async-errors"]);
    assert_eq!(f.kb_salience, Some(0.7));
    assert!(
        f.body.to_lowercase().contains("body"),
        "body extracted: {}",
        f.body
    );
    assert!(f.headings.contains("Heading"), "headings: {}", f.headings);
    assert!(f.headings.contains("Sub"), "headings: {}", f.headings);
    // The wrapped HTML carries the rendered anchor so the indexer's
    // link-edge pass sees the same DOM the serve path renders.
    assert!(
        html.contains(r#"href="./other.md""#),
        "rendered relative link present: {html}"
    );
}

#[test]
fn title_falls_back_to_first_h1() {
    let (f, _) = extract_markdown("# Just A Heading\n\ntext\n");
    assert_eq!(f.title.as_deref(), Some("Just A Heading"));
}

#[test]
fn inline_kb_prompt_template_is_extracted_for_indexing() {
    // The prompt is inline-template-only (no frontmatter prompt); it must be
    // picked up for the index (and stripped by the scrub on public serve).
    let src = "# Doc\n\n<template id=\"kb-prompt\">the secret prompt</template>\n\nbody\n";
    let (f, _) = extract_markdown(src);
    assert_eq!(f.prompt.as_deref(), Some("the secret prompt"));
}

#[test]
fn rendered_markdown_kb_prompt_is_stripped_by_export_scrub() {
    // P4 security invariant: a markdown artifact's inline kb-prompt survives
    // `render_page` (so it indexes) but the outbound/export scrub removes it
    // before any non-loopback serve or static share. render → scrub_export
    // must leave no trace of the template id OR the prompt text.
    use kb_core::markdown::render_page;
    use kb_core::scrub::scrub_export;
    let src =
        "---\ntitle: Secrety\n---\n# Doc\n\n<template id=\"kb-prompt\">SECRET PROMPT</template>\n\nvisible body\n";
    let page = render_page(src);
    assert!(
        page.contains(r#"<template id="kb-prompt">"#),
        "prompt present before scrub: {page}"
    );
    let scrubbed = scrub_export(&page, None);
    assert!(
        !scrubbed.contains("kb-prompt"),
        "template id gone: {scrubbed}"
    );
    assert!(
        !scrubbed.contains("SECRET PROMPT"),
        "prompt text gone: {scrubbed}"
    );
    assert!(
        scrubbed.contains("visible body"),
        "body retained: {scrubbed}"
    );
}
