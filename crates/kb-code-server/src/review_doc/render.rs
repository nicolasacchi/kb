//! `kbc-review/1` RENDER — the stored Markdown + its resolved cards, poured
//! into an operator-provided HTML template (V73-K1, design D9-a).
//!
//! This is the ONE place kb-code produces HTML from a review, and it is an
//! **export**, never a stored representation: the record stays the Markdown
//! document (D9-a — HTML is a permanent XSS surface, cannot be interdiffed
//! across re-reviews, and its references are dead text). Re-rendering the
//! same revision through the same template is byte-identical; re-rendering
//! it tomorrow may differ, because the CARDS are resolved against the
//! repository as it is now, and the output says so in its own provenance
//! line.
//!
//! # The template grammar
//!
//! A template is an ordinary HTML file the operator owns. A `{{name}}` whose
//! `name` is in [`PLACEHOLDERS`] is replaced by server-composed HTML;
//! **every other `{{…}}` is left byte-for-byte alone** and reported in
//! `unknown_placeholders`. That rule exists so a template's own CSS
//! (`@media {{`-free but full of braces) and any JS it ships are never
//! mangled by this renderer — and so an author who typos `{{sumary}}` is
//! told, rather than silently shipping a page with a hole.
//!
//! | placeholder | what it becomes |
//! |---|---|
//! | `{{title}}` | the review's title, escaped text |
//! | `{{meta}}` | provenance: repo, review id, patchset, revision, tier, when this HTML was rendered |
//! | `{{summary}}` | `summary_md`, rendered Markdown |
//! | `{{risk}}` | the risk level + why, or an explicit "not stated" |
//! | `{{reading_order}}` | chapters and stops, each stop naming its ref |
//! | `{{blocks}}` | the named sections, in [`crate::review_doc::BLOCK_NAMES`] order |
//! | `{{findings}}` | the findings table |
//! | `{{cards}}` | every resolved ref as a card, with its state, trust and caption |
//! | `{{flows}}` | named paths through the diff |
//! | `{{questions}}` | typed questions |
//! | `{{author}}` | the author block, including what was NOT considered |
//! | `{{omitted}}` | the stated omissions — an absence is never silent, not even in an export |
//! | `{{body}}` | the Markdown body, rendered |
//! | `{{pr_number}}` | the review's PR binding, or an honest `n/a` |
//! | `{{risk_score}}` | the document risk level and/or the report score, or `n/a` |
//! | `{{tags}}` | kb-tags value: `review`, `pr-<n>` when bound, the repo, then PR labels |
//! | `{{summary_text}}` | plain-text summary, HTML-escaped, capped at [`SUMMARY_TEXT_CAP`] characters |
//! | `{{repo}}` | the configured repo name, escaped |
//!
//! # Escaping
//!
//! Two rules, both absolute. **(a)** Every dynamic STRING goes through
//! [`esc`] — a caption, a path, a finding title, a ref body, a template
//! name. **(b)** Every Markdown body goes through
//! `kb_core::markdown::render_comment_fragment`, the crate's existing
//! UNTRUSTED-body renderer (`render.unsafe = false`), so raw HTML inside a
//! review document is escaped rather than passed through — the same posture
//! the SPA takes for comment bodies and the one D9's "Markdown renderer
//! contract" names. Nothing this module emits can execute; the only script
//! in the output is script the operator put in their own template.
//!
//! kb-code NEVER generates a `<template id="kb-prompt">` — that convention
//! is kb's (root invariant #5) and the kb side owns writing one.

use crate::review_doc::cards::Card;
use crate::review_doc::{Author, Chapter, DocFinding, Flow, Question, ReviewDoc, Risk};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One row of the rendered findings table.
///
/// A rendered review shows the findings the STORE holds after
/// reconciliation — with their minted slugs, their tombstones and any
/// human disposition — not the ones the document's front matter happened to
/// declare, which may carry no slug at all. [`FindingLine::from_doc`] is
/// the other direction, for a dry-run render of a document that has not
/// been composed yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingLine {
    pub slug: String,
    pub act: String,
    pub severity: String,
    pub blocking: bool,
    pub category: String,
    pub title: String,
    /// `path` or `path:12,14` — pre-composed by the caller so this module
    /// never re-derives a location format.
    pub location: String,
    pub superseded: bool,
    pub disposition: Option<String>,
}

impl FindingLine {
    /// A finding as a DOCUMENT declares it — no slug yet, no disposition,
    /// never superseded.
    pub fn from_doc(f: &DocFinding) -> FindingLine {
        let location = match &f.location.lines {
            Some(l) if !l.is_empty() => format!(
                "{}:{}",
                f.location.path,
                l.iter().map(i64::to_string).collect::<Vec<_>>().join(",")
            ),
            _ => f.location.path.clone(),
        };
        FindingLine {
            slug: f
                .slug
                .clone()
                .unwrap_or_else(|| "(minted on compose)".to_string()),
            act: f.act.clone(),
            severity: f.severity.clone(),
            blocking: f.blocking,
            category: f.category.clone(),
            title: f.title.clone(),
            location,
            superseded: false,
            disposition: None,
        }
    }
}

/// The closed placeholder set. Order is documentation order.
///
/// V73-K5 (gap 6) adds `pr_number` and `risk_score` — the two identifiers a
/// legacy artifact's header always carried that a rendered `kbc-review/1`
/// export could not yet name. Neither is a NEW document field: `pr_number`
/// is the review's own PR binding (`reviews.pr_number`, already stored by
/// the PR Room) and `risk_score` is the pre-K1 `report_json.risk_score`
/// lane the coverage table already names as that concept's real home —
/// `risk` stays deliberately level-plus-why, never a score, by design.
///
/// V76-R1c adds `tags`, `summary_text`, and `repo` so the built-in template
/// is a legal kb artifact (`kb-tags` / `kb-summary` / a named repo) without
/// a custom template. Existing placeholder names stay byte-identical.
pub const PLACEHOLDERS: [&str; 18] = [
    "title",
    "meta",
    "summary",
    "risk",
    "reading_order",
    "blocks",
    "findings",
    "cards",
    "flows",
    "questions",
    "author",
    "omitted",
    "body",
    "pr_number",
    "risk_score",
    "tags",
    "summary_text",
    "repo",
];

/// Cap on `{{summary_text}}` (the `kb-summary` meta). Truncated at a char
/// boundary, never mid-codepoint; a refusal would make the export unusable,
/// so this is a hard cap with an ellipsis rather than a 400.
pub const SUMMARY_TEXT_CAP: usize = 300;

/// The built-in template — what `render` uses when the operator names none.
/// Deliberately plain and dependency-free: it exists so `render` is usable
/// on day one and so the placeholder grammar has a worked example in the
/// repository, not to be anyone's house style.
pub const DEFAULT_TEMPLATE: &str = include_str!("../../templates/review-default.html");

/// Everything the renderer needs that is not the document.
pub struct RenderCtx<'a> {
    pub repo: &'a str,
    pub review_id: i64,
    pub title: &'a str,
    pub ps_number: i64,
    pub revision: i64,
    pub tier: &'a str,
    pub rendered_at: i64,
    pub omitted: &'a [String],
    /// V73-K5 — the review's own PR binding number, when it has one.
    pub pr_number: Option<i64>,
    /// V73-K5 — the pre-K1 `report_json.risk_score` lane's numeric value,
    /// when the review has an authored report carrying one. Independent of
    /// `doc.risk` (which has no numeric axis by design).
    pub risk_score: Option<f64>,
    /// V76-R1c — comma-separated kb-tags (`review`, `pr-<n>`, repo, labels).
    /// Already slug-shaped; [`render`] HTML-escapes it for the meta tag.
    pub tags: &'a str,
    /// V76-R1c — plain-text summary, already capped; [`render`] escapes it.
    pub summary_text: &'a str,
}

#[derive(Debug, Clone)]
pub struct Rendered {
    pub html: String,
    /// Every `{{…}}` the template used that this renderer does not know.
    /// Left untouched in `html`; reported so a typo is never silent.
    pub unknown_placeholders: Vec<String>,
}

/// Render `doc` + `cards` through `template`.
pub fn render(
    template: &str,
    doc: &ReviewDoc,
    findings: &[FindingLine],
    cards: &[Card],
    ctx: &RenderCtx<'_>,
) -> Rendered {
    let mut values: BTreeMap<&str, String> = BTreeMap::new();
    values.insert("title", esc(ctx.title));
    values.insert("meta", meta_html(ctx));
    values.insert("summary", md(&doc.summary_md));
    values.insert("risk", risk_html(doc.risk.as_ref()));
    values.insert("reading_order", reading_order_html(&doc.reading_order));
    values.insert("blocks", blocks_html(&doc.blocks));
    values.insert("findings", findings_html(findings));
    values.insert("cards", cards_html(cards));
    values.insert("flows", flows_html(&doc.flows));
    values.insert("questions", questions_html(&doc.questions));
    values.insert("author", author_html(doc.author.as_ref()));
    values.insert("omitted", omitted_html(ctx.omitted));
    values.insert("body", md(&doc.body_md));
    values.insert("pr_number", pr_number_html(ctx.pr_number));
    values.insert(
        "risk_score",
        risk_score_html(doc.risk.as_ref(), ctx.risk_score),
    );
    values.insert("tags", esc(ctx.tags));
    values.insert("summary_text", esc(ctx.summary_text));
    values.insert("repo", esc(ctx.repo));
    substitute(template, &values)
}

/// Collapse whitespace, trim, cap at [`SUMMARY_TEXT_CAP`]. Used for the
/// `kb-summary` meta — a Markdown body is the wrong shape for a one-line
/// description.
pub fn summary_text(summary_md: &str) -> String {
    let collapsed: String = summary_md.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SUMMARY_TEXT_CAP {
        return collapsed;
    }
    let mut out: String = collapsed
        .chars()
        .take(SUMMARY_TEXT_CAP.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

/// `review`, plus `pr-<n>` when bound, plus the repo (slashes folded to
/// dashes so kb's tag slugifier does not drop the owner), plus any PR
/// labels. Empty labels are skipped. Order is stable.
pub fn kb_tags(repo: &str, pr_number: Option<i64>, labels: &[String]) -> String {
    let mut tags = vec!["review".to_string()];
    if let Some(n) = pr_number {
        tags.push(format!("pr-{n}"));
    }
    let repo_tag = repo.replace(['/', ' '], "-").to_ascii_lowercase();
    if !repo_tag.is_empty() && !tags.iter().any(|t| t == &repo_tag) {
        tags.push(repo_tag);
    }
    for label in labels {
        let slug: String = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let slug = slug.trim_matches('-');
        if !slug.is_empty() && !tags.iter().any(|t| t == slug) {
            tags.push(slug.to_string());
        }
    }
    tags.join(",")
}

/// Replace every `{{known}}`; leave every other `{{…}}` byte-for-byte and
/// report it. Scans once, left to right, so a value that itself contains
/// `{{` is never re-scanned (a finding title of `{{title}}` renders as text,
/// not as a second substitution).
pub fn substitute(template: &str, values: &BTreeMap<&str, String>) -> Rendered {
    let mut html = String::with_capacity(template.len() * 2);
    let mut unknown: Vec<String> = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'{' && i + 1 < bytes.len() && bytes[i + 1] == b'{' {
            if let Some(close) = template[i + 2..].find("}}") {
                let name = &template[i + 2..i + 2 + close];
                let trimmed = name.trim();
                if let Some(v) = values.get(trimmed) {
                    html.push_str(v);
                    i = i + 2 + close + 2;
                    continue;
                }
                // Only report something that LOOKS like a placeholder —
                // a short, identifier-shaped token. A template's CSS/JS
                // braces are neither replaced nor reported as typos.
                if !trimmed.is_empty()
                    && trimmed.len() <= 40
                    && trimmed
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !unknown.iter().any(|u| u == trimmed)
                {
                    unknown.push(trimmed.to_string());
                }
            }
        }
        // Copy one whole UTF-8 char, never one byte, so the output is
        // always valid UTF-8.
        let ch = template[i..].chars().next().expect("in bounds");
        html.push(ch);
        i += ch.len_utf8();
    }
    Rendered {
        html,
        unknown_placeholders: unknown,
    }
}

// --- section builders -------------------------------------------------------

fn meta_html(ctx: &RenderCtx<'_>) -> String {
    format!(
        "<p class=\"kbc-meta\">{} · review {} · patchset {} · revision {} · tier {} \
         · rendered at {} (the cards below were resolved against the repository at that \
         moment; the stored record is the Markdown document, not this page)</p>",
        esc(ctx.repo),
        ctx.review_id,
        ctx.ps_number,
        ctx.revision,
        esc(ctx.tier),
        ctx.rendered_at,
    )
}

fn risk_html(risk: Option<&Risk>) -> String {
    match risk {
        Some(r) => format!(
            "<p class=\"kbc-risk kbc-risk--{}\"><strong>Risk: {}</strong> — {}</p>",
            esc(&r.level),
            esc(&r.level),
            esc(&r.why)
        ),
        None => "<p class=\"kbc-risk kbc-risk--unstated\">Risk: not stated by this document.</p>"
            .to_string(),
    }
}

fn reading_order_html(chapters: &[Chapter]) -> String {
    if chapters.is_empty() {
        return empty("No reading order in this document.");
    }
    let mut out = String::from("<ol class=\"kbc-reading-order\">");
    for c in chapters {
        let _ = write!(out, "<li><strong>{}</strong><ol>", esc(&c.chapter));
        for s in &c.stops {
            let _ = write!(out, "<li><code>{}</code>", esc(&s.r#ref));
            if let Some(why) = &s.why {
                let _ = write!(out, " — {}", esc(why));
            }
            out.push_str("</li>");
        }
        out.push_str("</ol></li>");
    }
    out.push_str("</ol>");
    out
}

fn blocks_html(blocks: &BTreeMap<String, String>) -> String {
    if blocks.is_empty() {
        return empty("No named sections in this document.");
    }
    let mut out = String::new();
    // BLOCK_NAMES order, not map order: the document reads the same way
    // every time regardless of how the author happened to type it.
    for name in crate::review_doc::BLOCK_NAMES {
        let Some(body) = blocks.get(name) else {
            continue;
        };
        let _ = write!(
            out,
            "<section class=\"kbc-block\" id=\"block-{}\"><h3>{}</h3>{}</section>",
            esc(name),
            esc(&name.replace('_', " ")),
            md(body)
        );
    }
    out
}

fn findings_html(findings: &[FindingLine]) -> String {
    if findings.is_empty() {
        return empty("This review reports no findings — an explicit claim, not an omission.");
    }
    let mut out = String::from(
        "<table class=\"kbc-findings\"><thead><tr><th>slug</th><th>act</th><th>severity</th>\
         <th>blocking</th><th>category</th><th>location</th><th>title</th><th>disposition</th>\
         </tr></thead><tbody>",
    );
    for f in findings {
        let _ = write!(
            out,
            "<tr class=\"kbc-finding kbc-finding--{}{}\"><td><code>{}</code></td><td>{}</td>\
             <td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td></tr>",
            esc(&f.severity),
            if f.superseded {
                " kbc-finding--superseded"
            } else {
                ""
            },
            esc(&f.slug),
            esc(&f.act),
            esc(&f.severity),
            if f.blocking { "yes" } else { "no" },
            esc(&f.category),
            esc(&f.location),
            esc(&f.title),
            esc(f.disposition.as_deref().unwrap_or("undecided")),
        );
    }
    out.push_str("</tbody></table>");
    out
}

fn cards_html(cards: &[Card]) -> String {
    if cards.is_empty() {
        return empty("This document cites nothing.");
    }
    let mut out = String::new();
    for c in cards {
        let _ = write!(
            out,
            "<figure class=\"kbc-card kbc-card--{}\"><figcaption><code>[[{}]]</code> \
             <span class=\"kbc-state\">{}</span>",
            esc(c.state),
            esc(&c.r#ref),
            esc(c.state)
        );
        if let Some(t) = c.trust {
            let _ = write!(out, " <span class=\"kbc-trust\">{}</span>", esc(t));
        }
        let _ = write!(
            out,
            "<span class=\"kbc-caption\">{}</span></figcaption>",
            esc(&c.caption)
        );
        if let Some(snippet) = &c.snippet {
            let start = c.snippet_start.unwrap_or(1);
            let _ = write!(
                out,
                "<pre class=\"kbc-snippet\" data-start=\"{start}\"><code>{}</code></pre>",
                esc(snippet)
            );
        }
        out.push_str("</figure>");
    }
    out
}

fn flows_html(flows: &[Flow]) -> String {
    if flows.is_empty() {
        return empty("No flows in this document.");
    }
    let mut out = String::from("<ul class=\"kbc-flows\">");
    for f in flows {
        let _ = write!(out, "<li><strong>{}</strong><ol>", esc(&f.name));
        for s in &f.steps {
            let _ = write!(out, "<li><code>{}</code></li>", esc(s));
        }
        out.push_str("</ol></li>");
    }
    out.push_str("</ul>");
    out
}

fn questions_html(questions: &[Question]) -> String {
    if questions.is_empty() {
        return empty("No open questions in this document.");
    }
    let mut out = String::from("<ul class=\"kbc-questions\">");
    for q in questions {
        let _ = write!(
            out,
            "<li class=\"kbc-question kbc-question--{}\"><span class=\"kbc-to\">{}</span> {}",
            esc(&q.to),
            esc(&q.to),
            esc(&q.ask)
        );
        if let Some(r) = &q.r#ref {
            let _ = write!(out, " <code>{}</code>", esc(r));
        }
        out.push_str("</li>");
    }
    out.push_str("</ul>");
    out
}

fn author_html(author: Option<&Author>) -> String {
    let Some(a) = author else {
        return empty("This document names no author.");
    };
    let mut out = format!(
        "<div class=\"kbc-author\"><p>{}{}{}</p>",
        esc(&a.kind),
        a.model
            .as_deref()
            .map(|m| format!(" · {}", esc(m)))
            .unwrap_or_default(),
        a.session_id
            .as_deref()
            .map(|s| format!(" · session {}", esc(s)))
            .unwrap_or_default(),
    );
    for (label, items) in [
        ("Considered", &a.considered),
        ("Not considered", &a.not_considered),
    ] {
        if items.is_empty() {
            continue;
        }
        let _ = write!(
            out,
            "<p class=\"kbc-considered\"><strong>{label}:</strong></p><ul>"
        );
        for i in items {
            let _ = write!(out, "<li>{}</li>", esc(i));
        }
        out.push_str("</ul>");
    }
    out.push_str("</div>");
    out
}

fn omitted_html(omitted: &[String]) -> String {
    if omitted.is_empty() {
        return empty("Every optional block is present.");
    }
    let mut out = String::from(
        "<p class=\"kbc-omitted\">This document omits, and therefore claims nothing about:</p><ul>",
    );
    for o in omitted {
        let _ = write!(out, "<li><code>{}</code></li>", esc(o));
    }
    out.push_str("</ul>");
    out
}

fn empty(msg: &str) -> String {
    format!("<p class=\"kbc-empty\">{}</p>", esc(msg))
}

/// The review's bound PR number, or an honest "n/a" — never a guess and
/// never `0`.
fn pr_number_html(pr_number: Option<i64>) -> String {
    match pr_number {
        Some(n) => esc(&n.to_string()),
        None => "n/a".to_string(),
    }
}

/// The document's own risk LEVEL, with the pre-K1 report lane's numeric
/// score appended when the review's report carries one — see
/// [`RenderCtx::risk_score`]'s doc for why that is a SEPARATE lane rather
/// than a new document field. "n/a" only when there is no risk at all to
/// report (the document names none and no score exists either).
fn risk_score_html(risk: Option<&Risk>, risk_score: Option<f64>) -> String {
    match (risk, risk_score) {
        (None, None) => "n/a".to_string(),
        (None, Some(score)) => esc(&format_score(score)),
        (Some(r), None) => esc(&r.level),
        (Some(r), Some(score)) => esc(&format!("{} ({})", r.level, format_score(score))),
    }
}

/// A numeric score, trimmed of a trailing `.0` when it is a whole number —
/// `report_json.risk_score` is author-supplied JSON and may be an integer
/// or a fraction.
fn format_score(score: f64) -> String {
    if score.fract() == 0.0 {
        format!("{score:.0}")
    } else {
        format!("{score}")
    }
}

/// Render an UNTRUSTED Markdown body. `kb_core::markdown::
/// render_comment_fragment` sets `render.unsafe = false`, so raw HTML in a
/// review document is ESCAPED rather than passed through — reusing the
/// crate's existing untrusted-body contract instead of configuring a second
/// renderer that could drift from it.
fn md(body: &str) -> String {
    kb_core::markdown::render_comment_fragment(body)
}

/// HTML-escape a dynamic string. Escapes the five characters that can leave
/// text context in either an element body or a quoted attribute — `'` and
/// `"` included, because several builders above interpolate into
/// `class="…"`/`data-…="…"`.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_doc::cards::STATE_ORPHAN;
    use crate::review_doc::parse;

    const FIXTURE: &str = include_str!("../../tests/fixtures/review-doc/acme-app-standard.md");

    /// The XSS corpus. Every one of these strings is put through a DIFFERENT
    /// interpolation site below; a regression in any single `esc` call site
    /// fails this test by name.
    const XSS: [&str; 6] = [
        "<script>alert(1)</script>",
        "\" onmouseover=\"alert(1)",
        "' onload='alert(1)",
        "<img src=x onerror=alert(1)>",
        "</pre><script>alert(1)</script><pre>",
        "javascript:alert(1)",
    ];

    fn ctx<'a>(title: &'a str, omitted: &'a [String]) -> RenderCtx<'a> {
        RenderCtx {
            repo: "acme-app",
            review_id: 7,
            title,
            ps_number: 1,
            revision: 1,
            tier: "standard",
            rendered_at: 0,
            omitted,
            pr_number: Some(42),
            risk_score: Some(0.5),
            tags: "review,pr-42,acme-app",
            summary_text: "s",
        }
    }

    #[test]
    fn no_interpolation_site_can_emit_a_script_tag_from_document_content() {
        for payload in XSS {
            let doc = ReviewDoc {
                summary_md: format!("plain {payload}"),
                risk: Some(Risk {
                    level: "low".into(),
                    why: payload.to_string(),
                }),
                reading_order: vec![Chapter {
                    chapter: payload.to_string(),
                    stops: vec![crate::review_doc::Stop {
                        r#ref: payload.to_string(),
                        why: Some(payload.to_string()),
                    }],
                }],
                blocks: BTreeMap::from([("context".to_string(), format!("text {payload}"))]),
                findings: None,
                flows: vec![Flow {
                    name: payload.to_string(),
                    steps: vec![payload.to_string()],
                }],
                questions: vec![Question {
                    to: "to_author".into(),
                    ask: payload.to_string(),
                    r#ref: Some(payload.to_string()),
                    answers: Some(payload.to_string()),
                }],
                author: Some(Author {
                    kind: "agent".into(),
                    model: Some(payload.to_string()),
                    session_id: Some(payload.to_string()),
                    considered: vec![payload.to_string()],
                    not_considered: vec![payload.to_string()],
                }),
                ci: vec![crate::review_doc::CiCheck {
                    name: payload.to_string(),
                    status: "success".into(),
                    url: Some(payload.to_string()),
                    observed_at: Some(0),
                }],
                body_md: format!("body {payload}"),
                body_line: 1,
            };
            let finding = DocFinding {
                slug: Some("f-1".into()),
                act: payload.to_string(),
                severity: payload.to_string(),
                category: payload.to_string(),
                blocking: true,
                title: payload.to_string(),
                rationale: payload.to_string(),
                recommendation: None,
                location: crate::review_findings::FindingLocationBody {
                    path: payload.to_string(),
                    kind: "single".into(),
                    lines: Some(vec![1]),
                    removed: false,
                },
                cites: vec![],
                supersedes: vec![],
                evidence: None,
            };
            let mut card = Card::orphan_for_test(payload, payload);
            card.snippet = Some(payload.to_string());
            card.snippet_start = Some(1);
            let omitted = vec![payload.to_string()];
            let line = FindingLine::from_doc(&finding);
            let out = render(
                DEFAULT_TEMPLATE,
                &doc,
                std::slice::from_ref(&line),
                std::slice::from_ref(&card),
                &ctx(payload, &omitted),
            );
            // The assertion is about the TAG boundary, and only that:
            // element-content text is allowed to contain any character that
            // cannot open a tag. `&lt;img src=x onerror=alert(1)&gt;`
            // legitimately CONTAINS "onerror=alert" and is perfectly inert,
            // and a bare apostrophe inside a `<p>` is just an apostrophe —
            // testing for those substrings would be testing the wrong
            // thing, and would fail on correct output.
            //
            // The ATTRIBUTE half of the proof is
            // `esc_never_leaves_a_character_that_can_close_a_tag_or_an_attribute`
            // below: every dynamic value this module interpolates into a
            // quoted attribute goes through `esc`, and `esc` is pinned to
            // emit none of `< > " '`.
            for tag in ["<script", "<img", "<iframe", "<svg", "<object", "<embed"] {
                assert!(
                    !out.html.contains(tag),
                    "payload {payload:?} opened a live {tag} tag"
                );
            }
            assert!(
                out.html.contains("&lt;script&gt;") || !payload.contains("<script>"),
                "payload {payload:?} vanished instead of being escaped"
            );
        }
    }

    #[test]
    fn esc_never_leaves_a_character_that_can_close_a_tag_or_an_attribute() {
        for payload in XSS {
            let e = esc(payload);
            for c in ['<', '>', '"', '\''] {
                assert!(
                    !e.contains(c),
                    "esc({payload:?}) left a bare {c:?} — every dynamic attribute value in \
                     this module goes through it, so one raw quote is an escape hatch out of \
                     `class=\"…\"`"
                );
            }
            assert!(
                e.contains("&lt;") || !payload.contains('<'),
                "esc({payload:?}) dropped the character instead of escaping it"
            );
        }
        // Ampersand first, so an already-escaped entity is not double-read.
        assert_eq!(esc("&lt;"), "&amp;lt;");
    }

    #[test]
    fn an_unknown_placeholder_is_left_verbatim_and_reported() {
        let out = substitute(
            "<i>{{summary}}</i> {{sumary}} .a{color:red} {{ summary }}",
            &BTreeMap::from([("summary", "OK".to_string())]),
        );
        assert_eq!(out.html, "<i>OK</i> {{sumary}} .a{color:red} OK");
        assert_eq!(out.unknown_placeholders, vec!["sumary"]);
    }

    #[test]
    fn template_css_braces_are_never_reported_or_mangled() {
        let css = "<style>@media (min-width:40em){.a{color:red}}</style>{{summary}}";
        let out = substitute(css, &BTreeMap::from([("summary", "S".to_string())]));
        assert_eq!(
            out.html,
            "<style>@media (min-width:40em){.a{color:red}}</style>S"
        );
        assert!(out.unknown_placeholders.is_empty());
    }

    #[test]
    fn a_substituted_value_is_never_rescanned() {
        let out = substitute(
            "{{summary}}",
            &BTreeMap::from([("summary", "{{body}}".to_string())]),
        );
        assert_eq!(out.html, "{{body}}");
        assert!(out.unknown_placeholders.is_empty());
    }

    #[test]
    fn the_default_template_uses_only_declared_placeholders() {
        let out = substitute(
            DEFAULT_TEMPLATE,
            &PLACEHOLDERS
                .iter()
                .map(|p| (*p, String::new()))
                .collect::<BTreeMap<_, _>>(),
        );
        assert!(
            out.unknown_placeholders.is_empty(),
            "the shipped template names placeholders this renderer does not fill: {:?}",
            out.unknown_placeholders
        );
    }

    #[test]
    fn every_declared_placeholder_is_filled_by_render() {
        let doc = parse(FIXTURE).expect("parses");
        let findings: Vec<FindingLine> = doc
            .findings
            .clone()
            .unwrap_or_default()
            .iter()
            .map(FindingLine::from_doc)
            .collect();
        let omitted = crate::review_doc::omitted_blocks(&doc);
        let probe: String = PLACEHOLDERS
            .iter()
            .map(|p| format!("[{p}={{{{{p}}}}}]"))
            .collect();
        let out = render(&probe, &doc, &findings, &[], &ctx("t", &omitted));
        assert!(out.unknown_placeholders.is_empty());
        for p in PLACEHOLDERS {
            assert!(
                !out.html.contains(&format!("[{p}={{{{{p}}}}}]")),
                "placeholder {p:?} is declared but `render` never fills it"
            );
        }
    }

    #[test]
    fn raw_html_inside_a_markdown_body_is_escaped_not_passed_through() {
        let html = md("hello <script>alert(1)</script> world");
        assert!(!html.contains("<script>"), "{html}");
    }

    #[test]
    fn an_orphan_card_renders_its_reason_and_no_position() {
        let card = Card::orphan_for_test("code:gone.rb:1", "gone.rb does not exist at patchset 1");
        let html = cards_html(std::slice::from_ref(&card));
        assert!(html.contains("kbc-card--orphan"), "{html}");
        assert!(html.contains("does not exist"), "{html}");
        assert!(!html.contains("kbc-snippet"), "{html}");
        assert_eq!(card.state, STATE_ORPHAN);
    }

    // --- V73-K5 gap 6: {{pr_number}} / {{risk_score}} ----------------------

    #[test]
    fn pr_number_is_the_bound_number_or_an_honest_n_a() {
        assert_eq!(pr_number_html(Some(15476)), "15476");
        assert_eq!(pr_number_html(None), "n/a");
    }

    #[test]
    fn risk_score_combines_the_document_level_with_the_report_score_or_says_n_a() {
        let high = Risk {
            level: "high".to_string(),
            why: "touches billing".to_string(),
        };
        assert_eq!(risk_score_html(None, None), "n/a");
        assert_eq!(risk_score_html(Some(&high), None), "high");
        assert_eq!(risk_score_html(None, Some(0.5)), "0.5");
        assert_eq!(risk_score_html(Some(&high), Some(7.0)), "high (7)");
    }

    #[test]
    fn every_render_names_the_pr_number_and_risk_score_placeholders() {
        let doc = ReviewDoc {
            summary_md: "s".into(),
            risk: None,
            reading_order: vec![],
            blocks: BTreeMap::new(),
            findings: None,
            flows: vec![],
            questions: vec![],
            author: None,
            ci: vec![],
            body_md: "b".into(),
            body_line: 1,
        };
        let omitted = Vec::new();
        let mut ctx = ctx("t", &omitted);
        ctx.pr_number = Some(15476);
        ctx.risk_score = None;
        let out = render(DEFAULT_TEMPLATE, &doc, &[], &[], &ctx);
        assert!(out.html.contains("15476"), "{}", out.html);
        assert!(out.unknown_placeholders.is_empty());
    }

    #[test]
    fn kb_tags_are_review_pr_repo_then_labels() {
        assert_eq!(kb_tags("acme-app", None, &[]), "review,acme-app");
        assert_eq!(
            kb_tags(
                "acme/widget",
                Some(7),
                &["Needs Review".into(), "bug".into()]
            ),
            "review,pr-7,acme-widget,needs-review,bug"
        );
    }

    #[test]
    fn summary_text_collapses_whitespace_and_caps_at_300() {
        assert_eq!(summary_text("  hello   world\n"), "hello world");
        let long = "word ".repeat(200);
        let out = summary_text(&long);
        assert!(
            out.chars().count() <= SUMMARY_TEXT_CAP,
            "{}",
            out.chars().count()
        );
        assert!(out.ends_with('…'), "{out}");
    }

    #[test]
    fn the_default_template_emits_kb_legal_metas_and_stable_section_ids() {
        let doc = parse(FIXTURE).expect("parses");
        let omitted = crate::review_doc::omitted_blocks(&doc);
        let mut ctx = ctx("t", &omitted);
        ctx.tags = "review,pr-42,acme-app";
        ctx.summary_text = "hello <script>";
        let out = render(DEFAULT_TEMPLATE, &doc, &[], &[], &ctx);
        assert!(
            out.html
                .contains(r#"<meta name="kb-category" content="review">"#),
            "{}",
            out.html
        );
        assert!(
            out.html
                .contains(r#"<meta name="kb-tags" content="review,pr-42,acme-app">"#),
            "{}",
            out.html
        );
        assert!(
            out.html
                .contains(r#"<meta name="kb-summary" content="hello &lt;script&gt;">"#),
            "summary_text must be escaped: {}",
            out.html
        );
        for id in ["summary", "findings", "verdict", "risk", "omitted"] {
            assert!(
                out.html.contains(&format!(r#"id="{id}""#)),
                "missing stable section id {id}"
            );
        }
        assert!(!out.html.contains("<base "), "{}", out.html);
        assert!(!out.html.contains(r#"target="_top""#), "{}", out.html);
        assert!(
            out.unknown_placeholders.is_empty(),
            "{:?}",
            out.unknown_placeholders
        );
    }
}
