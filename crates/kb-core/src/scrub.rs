//! Outbound scrubbing primitive.
//!
//! Two transformations applied to artifact HTML before it leaves the
//! trusted boundary:
//!
//! 1. `strip_kb_prompt` — drops `<template id="kb-prompt">` via lol-html.
//!    Cheap and surgical; only the matched element is removed.
//! 2. `redactions` — ordered regex `pattern → replacement` rules applied
//!    against the post-strip body.
//!
//! Two callers share this exact transform:
//! - the daemon serve-path (`kb-server`), which scrubs an artifact when
//!   the request looks non-loopback (a tunnel / reverse proxy in front);
//! - the static-export engine (`kb_core::share`), which scrubs at export
//!   time because the published bytes bypass the daemon entirely.
//!
//! The *decision* of WHEN to scrub on the serve-path
//! (`kb_server::scrub::looks_non_loopback`) stays in `kb-server` — it
//! depends on the HTTP middleware's loopback rule. This module is the
//! pure transform plus the compiled-rule cache.

use crate::config::OutboundSection;
use lol_html::html_content::ContentType;
use lol_html::{element, HtmlRewriter, Settings};
use regex::Regex;
use std::sync::Arc;

/// Compiled outbound rules for one kb. Built once (via [`OutboundCache::from_section`])
/// and reused per scrub. Invalid patterns are dropped at build time.
pub struct OutboundCache {
    pub strip_kb_prompt: bool,
    pub rules: Vec<CompiledRule>,
}

pub struct CompiledRule {
    pub regex: Regex,
    pub replacement: String,
}

impl OutboundCache {
    /// Build from an `OutboundSection`, dropping any regex that fails
    /// to compile (with a warn log). Returns `None` when there's
    /// nothing to do (no rules + strip_kb_prompt = false).
    pub fn from_section(section: &OutboundSection) -> Option<Arc<Self>> {
        let mut rules = Vec::with_capacity(section.redactions.len());
        for r in &section.redactions {
            match Regex::new(&r.pattern) {
                Ok(rx) => rules.push(CompiledRule {
                    regex: rx,
                    replacement: r.replacement.clone(),
                }),
                Err(e) => tracing::warn!(
                    pattern = %r.pattern, error = %e,
                    "outbound regex failed to compile — skipping"
                ),
            }
        }
        if !section.strip_kb_prompt && rules.is_empty() {
            return None;
        }
        Some(Arc::new(Self {
            strip_kb_prompt: section.strip_kb_prompt,
            rules,
        }))
    }
}

/// Apply `cache`'s rules to the HTML body. `strip_kb_prompt` runs
/// first via lol-html (one streaming pass); regex redactions then run
/// in declaration order against the result.
pub fn scrub(html: &str, cache: &OutboundCache) -> String {
    let stage1 = if cache.strip_kb_prompt {
        strip_kb_prompt_template(html)
    } else {
        html.to_string()
    };
    apply_regex_rules(stage1, cache)
}

fn strip_kb_prompt_template(html: &str) -> String {
    let mut output: Vec<u8> = Vec::with_capacity(html.len());
    let mut rewriter = HtmlRewriter::new(
        Settings {
            element_content_handlers: vec![element!(r#"template[id="kb-prompt"]"#, |el| {
                el.replace("", ContentType::Html);
                Ok(())
            })],
            ..Settings::default()
        },
        |c: &[u8]| output.extend_from_slice(c),
    );
    if rewriter.write(html.as_bytes()).is_err() {
        return html.to_string();
    }
    if rewriter.end().is_err() {
        return html.to_string();
    }
    String::from_utf8(output).unwrap_or_else(|_| html.to_string())
}

fn apply_regex_rules(mut s: String, cache: &OutboundCache) -> String {
    for rule in &cache.rules {
        s = rule
            .regex
            .replace_all(&s, rule.replacement.as_str())
            .into_owned();
    }
    s
}

/// Export scrub for `kb share`. **Always** strips `<template
/// id="kb-prompt">` — a static share is more exposed than the live daemon
/// (it's served by a third party with no daemon in the path), so the
/// generation prompt must never ride along, regardless of the kb's
/// `[outbound] strip_kb_prompt` setting. Then applies the kb's redaction
/// rules if any are configured. The serve-path [`scrub`] stays gated on
/// config; this is the share-only variant (doc 23 §06).
pub fn scrub_export(html: &str, rules: Option<&OutboundCache>) -> String {
    let stripped = strip_kb_prompt_template(html);
    match rules {
        Some(cache) => apply_regex_rules(stripped, cache),
        None => stripped,
    }
}

/// Export scrub for RAW Markdown source (the native single-page `.md`
/// download). Unlike [`scrub_export`], it does **not** run the HTML
/// template stripper: Markdown carries no `<template id="kb-prompt">` to
/// remove (that convention is HTML-artifact-only), and running lol-html
/// over Markdown source would re-serialise it — mangling literal `<`/`&`
/// and collapsing whitespace the author meant to keep. Only the kb's regex
/// redactions apply, so a `.md` with no outbound rules is returned
/// byte-for-byte.
pub fn scrub_export_markdown(src: &str, rules: Option<&OutboundCache>) -> String {
    match rules {
        Some(cache) => apply_regex_rules(src.to_string(), cache),
        None => src.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_with(strip: bool, rules: Vec<(&str, &str)>) -> OutboundCache {
        OutboundCache {
            strip_kb_prompt: strip,
            rules: rules
                .into_iter()
                .map(|(p, r)| CompiledRule {
                    regex: Regex::new(p).unwrap(),
                    replacement: r.to_string(),
                })
                .collect(),
        }
    }

    // invariant:5 kb-prompt
    #[test]
    fn scrub_strips_kb_prompt_template() {
        let html = r##"<html><body>
            <h1>Hi</h1>
            <template id="kb-prompt">SECRET PROMPT TEXT</template>
            <p>visible</p>
        </body></html>"##;
        let cache = cache_with(true, vec![]);
        let out = scrub(html, &cache);
        assert!(!out.contains("SECRET PROMPT TEXT"));
        assert!(out.contains("<p>visible</p>"));
        assert!(out.contains("<h1>Hi</h1>"));
    }

    #[test]
    fn scrub_applies_regex_redactions_in_order() {
        let html = "Email: alice@example.com, ID: 1234-5678";
        let cache = cache_with(
            false,
            vec![
                (r"[\w.+-]+@[\w-]+\.[\w.-]+", "<email>"),
                (r"\d{4}-\d{4}", "<id>"),
            ],
        );
        let out = scrub(html, &cache);
        assert_eq!(out, "Email: <email>, ID: <id>");
    }

    #[test]
    fn scrub_combines_strip_then_regex() {
        let html = r##"<template id="kb-prompt">prompt</template>
            See alice@example.com"##;
        let cache = cache_with(true, vec![(r"[\w.+-]+@[\w-]+\.[\w.-]+", "<email>")]);
        let out = scrub(html, &cache);
        assert!(!out.contains("prompt"));
        assert!(!out.contains("alice@example.com"));
        assert!(out.contains("<email>"));
    }

    #[test]
    fn scrub_noop_when_no_rules_and_no_strip() {
        let html = "<p>hi</p>";
        let cache = cache_with(false, vec![]);
        // (this exact configuration would have OutboundCache::from_section
        // return None, but the scrub path still no-ops.)
        let out = scrub(html, &cache);
        assert_eq!(out, html);
    }

    #[test]
    fn scrub_export_always_strips_even_without_config() {
        // The share variant must drop the kb-prompt with NO outbound config
        // at all (None) — the serve-path `scrub` would not.
        let html = r##"<p>keep</p><template id="kb-prompt">LEAK</template>"##;
        let out = scrub_export(html, None);
        assert!(!out.contains("LEAK"));
        assert!(out.contains("keep"));
    }

    #[test]
    fn scrub_export_also_applies_redactions() {
        let html = r##"<template id="kb-prompt">x</template>Email a@b.com"##;
        let cache = cache_with(false, vec![(r"[\w.+-]+@[\w-]+\.[\w.-]+", "<email>")]);
        let out = scrub_export(html, Some(&cache));
        assert!(!out.contains("x</template>") && !out.contains("a@b.com"));
        assert!(out.contains("<email>"));
    }

    #[test]
    fn scrub_export_markdown_is_byteperfect_without_rules() {
        // Raw `.md` source must survive untouched when there are no outbound
        // rules — no lol-html re-serialisation of `<`, `&`, or whitespace.
        let md = "# Title\n\nA `<tag>` & an & entity.\n\n- a\n- b\n";
        assert_eq!(scrub_export_markdown(md, None), md);
    }

    #[test]
    fn scrub_export_markdown_applies_redactions_only() {
        // Redactions apply; the kb-prompt stripper does NOT (no HTML parse).
        let md = "See alice@example.com — kept literal: <template id=\"kb-prompt\">x</template>";
        let cache = cache_with(true, vec![(r"[\w.+-]+@[\w-]+\.[\w.-]+", "<email>")]);
        let out = scrub_export_markdown(md, Some(&cache));
        assert!(!out.contains("alice@example.com"));
        assert!(out.contains("<email>"));
        // The literal template text is part of the prose and stays verbatim.
        assert!(out.contains(r#"<template id="kb-prompt">x</template>"#));
    }
}
