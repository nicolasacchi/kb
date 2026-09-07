//! V73-K3 — `kbc-legacy-import/1`: the one-shot migration from a LEGACY
//! review HTML artifact to a `kbc-review/1` document plus a findings v2
//! sidecar (design D9's "the legacy artifacts migrate through an
//! agent-layer one-shot skill", D9-a's "`review import-html` extracts only
//! the embedded machine block and refuses with a reason when absent").
//!
//! # Only the JSON block. Never the prose.
//!
//! D9-a is a hard rule and this module exists to make it structural: the
//! importer reads ONE `<script type="application/json">` block out of the
//! artifact and nothing else. It does not walk the DOM, it does not read
//! headings, it does not "recover" a finding from a `<section>` that looks
//! like one. The reason is the same one the whole crate keeps restating: a
//! scraped finding is a GUESS wearing a finding's clothes, and a guessed
//! finding inherits a human's disposition, a slug, and a place in a GitHub
//! thread. An artifact with no machine block is REFUSED, by name, with the
//! block ids that were looked for — a refusal an operator can act on, where
//! a partial scrape is a mess they discover months later.
//!
//! [`extract_block`] is a byte scan over the raw HTML for exactly that
//! element, deliberately not an HTML parse: this crate's own recorded
//! refusal is "no HTML scraping in the daemon" (D21), and a scan that can
//! only ever find one `<script type="application/json">` cannot grow into
//! one.
//!
//! # There is no route
//!
//! The artifact is a file on the operator's box and the mapping is pure, so
//! this ships as `kb-code review import-legacy <artifact.html>` — a CLI
//! verb calling this module as a library — and NOT as an HTTP surface.
//! Uploading a file to the daemon to have it transformed and handed back
//! would add a mutation-shaped route that mutates nothing, and would put
//! artifact bytes on a wire for no gain.
//!
//! # The mapping is tolerant in ONE direction only
//!
//! The legacy block is somebody else's schema and it varies. Key ALIASES
//! are accepted ([`SUMMARY_KEYS`] and friends); VALUES outside a closed
//! kbc vocabulary are never silently coerced — they are mapped through a
//! stated substitution that appears in `notes`, or, with
//! [`ImportOptions::strict`], refused. A finding with no usable path is
//! SKIPPED with a reason rather than given one, because K1's own
//! `finding_no_location` lint would reject it anyway and a fabricated path
//! is worse than an honest omission.

use crate::review_doc::DocFinding;
use crate::review_findings::FindingLocationBody;
use serde::Serialize;
use serde_json::Value;

pub const SCHEMA: &str = "kbc-legacy-import/1";

/// The `id` attributes this importer will accept on the machine block, in
/// probe order. A CLOSED list: a new one is a deliberate edit here, and an
/// artifact carrying none of them is refused NAMING this list, so the
/// operator can see exactly what was looked for.
pub const BLOCK_IDS: &[&str] = &[
    "kb-review-data",
    "review-data",
    "pr-review-data",
    "kbc-review",
    "review-json",
];

/// Front-matter `summary_md` sources, in precedence order.
pub const SUMMARY_KEYS: &[&str] = &["summary_md", "summary", "overview", "abstract"];
/// Finding-list sources, in precedence order.
pub const FINDINGS_KEYS: &[&str] = &["findings", "issues", "items", "comments"];
/// Per-finding title sources.
pub const TITLE_KEYS: &[&str] = &["title", "summary", "headline", "name"];
/// Per-finding rationale sources.
pub const RATIONALE_KEYS: &[&str] = &["rationale", "detail", "details", "description", "body"];
/// Per-finding recommendation sources.
pub const RECOMMENDATION_KEYS: &[&str] = &["recommendation", "fix", "suggestion", "remedy"];
/// Per-finding path sources.
pub const PATH_KEYS: &[&str] = &["path", "file", "filename", "location"];

/// The kbc severity vocabulary (unchanged from `kbc-findings/1`).
pub const SEVERITIES: &[&str] = &["blocker", "concern", "ok"];
/// The findings-v2 category vocabulary.
pub const CATEGORIES: &[&str] = &[
    "correctness",
    "security",
    "performance",
    "design",
    "tests",
    "docs",
    "style",
    "other",
];

/// Legacy severity spellings this importer knows how to map, beyond the
/// three that already ARE the vocabulary. Everything else is substituted
/// with a stated note (or refused under `strict`).
pub const SEVERITY_ALIASES: &[(&str, &str)] = &[
    ("critical", "blocker"),
    ("high", "blocker"),
    ("blocking", "blocker"),
    ("must-fix", "blocker"),
    ("medium", "concern"),
    ("warning", "concern"),
    ("warn", "concern"),
    ("minor", "concern"),
    ("low", "concern"),
    ("nit", "ok"),
    ("nitpick", "ok"),
    ("info", "ok"),
    ("note", "ok"),
    ("praise", "ok"),
    ("pass", "ok"),
];

/// Why the import refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ImportError {
    /// No `<script type="application/json">` with a known id.
    NoBlock {
        looked_for: Vec<String>,
        hint: String,
    },
    /// The block was found but is not JSON.
    BlockNotJson { block_id: String, detail: String },
    /// The block parsed but carries nothing this importer can map.
    Empty { block_id: String, hint: String },
    /// `strict` was requested and a value fell outside a closed vocabulary.
    Strict { detail: String },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoBlock { looked_for, hint } => write!(
                f,
                "no embedded machine block: looked for a <script type=\"application/json\"> with \
                 id in {looked_for:?}. {hint}"
            ),
            Self::BlockNotJson { block_id, detail } => {
                write!(
                    f,
                    "the <script id=\"{block_id}\"> block is not JSON: {detail}"
                )
            }
            Self::Empty { block_id, hint } => {
                write!(
                    f,
                    "the <script id=\"{block_id}\"> block carries nothing mappable. {hint}"
                )
            }
            Self::Strict { detail } => write!(f, "strict mode refused the import: {detail}"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Refuse rather than substitute when a value falls outside a closed
    /// vocabulary.
    pub strict: bool,
    /// The tier to declare on the produced document. `standard` by
    /// default — a legacy artifact always has a summary, and a risk line is
    /// synthesised from its verdict when it has one.
    pub tier: Option<String>,
}

/// A finding the importer would not emit, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedFinding {
    pub index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub reason: String,
}

/// One stated substitution — the mapping table, as DATA, so `--json`
/// carries the same table the skill's documentation prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MappingNote {
    pub field: &'static str,
    pub from: String,
    pub to: String,
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ImportOut {
    pub schema: &'static str,
    /// The block this came from, named so the record is auditable.
    pub block_id: String,
    pub block_bytes: usize,
    pub tier: String,
    /// The kbc-review/1 document — front matter plus body, ready for
    /// `kb-code review compose --doc`.
    pub doc_md: String,
    /// The sidecar, ready for `--findings`. Same shape
    /// `review_pseudo::render_findings_json` emits.
    pub findings: Vec<DocFinding>,
    pub mapping: Vec<MappingNote>,
    pub skipped: Vec<SkippedFinding>,
    pub notes: Vec<String>,
}

/// Find the machine block. A byte scan, NOT an HTML parse — see the module
/// doc. Returns `(block_id, json_text)`.
pub fn extract_block(html: &str) -> Result<(String, String), ImportError> {
    for id in BLOCK_IDS {
        if let Some(found) = scan_script(html, id) {
            return Ok(((*id).to_string(), found));
        }
    }
    Err(ImportError::NoBlock {
        looked_for: BLOCK_IDS.iter().map(|s| s.to_string()).collect(),
        hint: "this artifact carries no machine record. Its prose is deliberately NOT scraped \
               (design D9-a) — a scraped finding is a guess that would inherit a slug, a \
               disposition and a GitHub thread. Re-review the PR instead, or hand-write the \
               kbc-review/1 document."
            .into(),
    })
}

/// One `<script …>` whose attributes contain both `application/json` and
/// `id="<id>"`, in either attribute order, single or double quoted. Returns
/// its inner text.
fn scan_script(html: &str, id: &str) -> Option<String> {
    let mut cursor = 0usize;
    while let Some(rel) = html[cursor..].find("<script") {
        let open_start = cursor + rel;
        let Some(gt_rel) = html[open_start..].find('>') else {
            return None;
        };
        let open_end = open_start + gt_rel;
        let attrs = &html[open_start..open_end];
        let close_rel = html[open_end + 1..].find("</script")?;
        let body_end = open_end + 1 + close_rel;
        let matches_id =
            attrs.contains(&format!("id=\"{id}\"")) || attrs.contains(&format!("id='{id}'"));
        if matches_id && attrs.contains("application/json") {
            return Some(html[open_end + 1..body_end].trim().to_string());
        }
        cursor = body_end;
    }
    None
}

fn first_str<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
        .filter(|s| !s.trim().is_empty())
}

fn first_array<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_array()))
}

/// The whole mapping, pure and total. `html` in, a document + sidecar out.
pub fn import(html: &str, opts: &ImportOptions) -> Result<ImportOut, ImportError> {
    let (block_id, json) = extract_block(html)?;
    let root: Value = serde_json::from_str(&json).map_err(|e| ImportError::BlockNotJson {
        block_id: block_id.clone(),
        detail: e.to_string(),
    })?;

    let summary = first_str(&root, SUMMARY_KEYS).map(str::to_string);
    let raw_findings = first_array(&root, FINDINGS_KEYS)
        .cloned()
        .unwrap_or_default();
    if summary.is_none() && raw_findings.is_empty() {
        return Err(ImportError::Empty {
            block_id,
            hint: format!(
                "no summary (looked for {SUMMARY_KEYS:?}) and no finding list (looked for \
                 {FINDINGS_KEYS:?})"
            ),
        });
    }

    let mut notes: Vec<String> = Vec::new();
    let mut mapping: Vec<MappingNote> = Vec::new();
    let mut skipped: Vec<SkippedFinding> = Vec::new();
    let mut findings: Vec<DocFinding> = Vec::new();

    for (index, raw) in raw_findings.iter().enumerate() {
        match map_finding(raw, opts, &mut mapping)? {
            Ok(f) => findings.push(f),
            Err(reason) => skipped.push(SkippedFinding {
                index,
                title: first_str(raw, TITLE_KEYS).map(str::to_string),
                reason,
            }),
        }
    }

    let summary_md = summary.unwrap_or_else(|| {
        notes.push(
            "the block carried no summary — the document's `summary_md` states that plainly \
             rather than inventing one"
                .into(),
        );
        "This document was migrated from a legacy review artifact that carried no summary. \
         The findings below are the artifact's own machine record."
            .to_string()
    });

    let risk = risk_from(&root);
    let tier = opts.tier.clone().unwrap_or_else(|| {
        if risk.is_some() {
            "standard".into()
        } else {
            "minimal".into()
        }
    });

    if !skipped.is_empty() {
        notes.push(format!(
            "{} finding(s) were NOT migrated — every one is listed in `skipped` with its \
             reason. None was given a fabricated location.",
            skipped.len()
        ));
    }
    notes.push(format!(
        "only the <script id=\"{block_id}\"> machine block was read; the artifact's prose was \
         not scraped (design D9-a)"
    ));

    let doc_md = render_document(&summary_md, risk.as_ref(), &block_id, &findings, &skipped);

    Ok(ImportOut {
        schema: SCHEMA,
        block_bytes: json.len(),
        block_id,
        tier,
        doc_md,
        findings,
        mapping,
        skipped,
        notes,
    })
}

/// The legacy verdict/risk line, when the block carries one. `risk` may be
/// a bare level string or a `{level, why}` object; a verdict is mapped to a
/// level through a stated table.
fn risk_from(root: &Value) -> Option<(String, String)> {
    if let Some(obj) = root.get("risk").and_then(|r| r.as_object()) {
        let level = obj.get("level").and_then(|l| l.as_str())?;
        let why = obj
            .get("why")
            .and_then(|w| w.as_str())
            .unwrap_or("carried over from the legacy artifact's own risk block");
        return Some((normalise_risk(level)?.to_string(), why.to_string()));
    }
    if let Some(level) = root.get("risk").and_then(|r| r.as_str()) {
        return Some((
            normalise_risk(level)?.to_string(),
            "carried over from the legacy artifact's own risk field".into(),
        ));
    }
    let verdict = first_str(root, &["verdict", "recommendation", "conclusion"])?;
    let level = match verdict.trim().to_ascii_lowercase().as_str() {
        "request-changes" | "request_changes" | "changes-requested" | "reject" => "high",
        "comment" | "neutral" => "medium",
        "approve" | "approved" | "lgtm" => "low",
        _ => return None,
    };
    Some((
        level.to_string(),
        format!("derived from the legacy artifact's verdict {verdict:?}"),
    ))
}

fn normalise_risk(s: &str) -> Option<&'static str> {
    match s.trim().to_ascii_lowercase().as_str() {
        "low" => Some("low"),
        "medium" | "moderate" => Some("medium"),
        "high" | "critical" => Some("high"),
        _ => None,
    }
}

/// `Ok(Ok(finding))` mapped · `Ok(Err(reason))` skipped · `Err(_)` strict
/// refusal.
#[allow(clippy::type_complexity)]
fn map_finding(
    raw: &Value,
    opts: &ImportOptions,
    mapping: &mut Vec<MappingNote>,
) -> Result<Result<DocFinding, String>, ImportError> {
    let Some(title) = first_str(raw, TITLE_KEYS) else {
        return Ok(Err(format!("no title (looked for {TITLE_KEYS:?})")));
    };
    let Some(path) = first_str(raw, PATH_KEYS) else {
        return Ok(Err(format!(
            "no location path (looked for {PATH_KEYS:?}) — a finding with no location fails \
             kbc-review/1's own `finding_no_location` lint, and inventing one would be worse \
             than omitting it"
        )));
    };

    let raw_sev = first_str(raw, &["severity", "level", "impact"]).unwrap_or("concern");
    let severity = map_severity(raw_sev, opts, mapping)?;
    let raw_cat = first_str(raw, &["category", "kind", "type"]);
    let category = map_category(raw_cat, opts, mapping)?;

    let lines = raw
        .get("line")
        .and_then(|l| l.as_i64())
        .map(|l| vec![l])
        .or_else(|| {
            raw.get("lines").and_then(|l| l.as_array()).map(|a| {
                a.iter()
                    .filter_map(serde_json::Value::as_i64)
                    .collect::<Vec<i64>>()
            })
        })
        .filter(|v| !v.is_empty());

    // `act` is a v2 axis the legacy shape has no column for. It is DERIVED
    // from severity through one stated rule rather than defaulted silently:
    // an `ok` row is a note, everything else is an issue.
    let act = if severity == "ok" { "note" } else { "issue" };

    Ok(Ok(DocFinding {
        // No slug: the ledger mints one. Carrying a legacy id across would
        // claim an identity in a namespace that never minted it (D9's slug
        // rule) — the legacy id is preserved in the rationale instead.
        slug: None,
        act: act.to_string(),
        severity: severity.to_string(),
        category: category.to_string(),
        blocking: severity == "blocker",
        title: title.to_string(),
        rationale: rationale_with_provenance(raw),
        recommendation: first_str(raw, RECOMMENDATION_KEYS).map(str::to_string),
        location: FindingLocationBody {
            path: path.to_string(),
            kind: if lines.is_some() {
                "lines".into()
            } else {
                "whole_file".into()
            },
            lines,
            removed: false,
        },
        cites: Vec::new(),
        supersedes: Vec::new(),
        evidence: None,
    }))
}

/// The rationale, plus the legacy id when the block carried one — so a
/// migrated finding can still be traced back to the artifact it came from
/// without claiming that id as a kbc slug.
fn rationale_with_provenance(raw: &Value) -> String {
    let base = first_str(raw, RATIONALE_KEYS).unwrap_or("").to_string();
    match first_str(raw, &["id", "slug", "ref"]) {
        Some(id) => {
            if base.is_empty() {
                format!("(migrated from legacy finding `{id}`)")
            } else {
                format!("{base}\n\n(migrated from legacy finding `{id}`)")
            }
        }
        None => base,
    }
}

fn map_severity(
    raw: &str,
    opts: &ImportOptions,
    mapping: &mut Vec<MappingNote>,
) -> Result<&'static str, ImportError> {
    let lower = raw.trim().to_ascii_lowercase();
    if let Some(s) = SEVERITIES.iter().find(|s| **s == lower) {
        return Ok(s);
    }
    if let Some((_, to)) = SEVERITY_ALIASES.iter().find(|(from, _)| *from == lower) {
        note_mapping(mapping, "severity", raw, to);
        return Ok(to);
    }
    if opts.strict {
        return Err(ImportError::Strict {
            detail: format!("severity {raw:?} is outside {SEVERITIES:?} and has no declared alias"),
        });
    }
    note_mapping(mapping, "severity", raw, "concern");
    Ok("concern")
}

fn map_category(
    raw: Option<&str>,
    opts: &ImportOptions,
    mapping: &mut Vec<MappingNote>,
) -> Result<&'static str, ImportError> {
    let Some(raw) = raw else { return Ok("other") };
    let lower = raw.trim().to_ascii_lowercase();
    if let Some(c) = CATEGORIES.iter().find(|c| **c == lower) {
        return Ok(c);
    }
    if opts.strict {
        return Err(ImportError::Strict {
            detail: format!("category {raw:?} is outside {CATEGORIES:?}"),
        });
    }
    note_mapping(mapping, "category", raw, "other");
    Ok("other")
}

fn note_mapping(mapping: &mut Vec<MappingNote>, field: &'static str, from: &str, to: &str) {
    if let Some(row) = mapping
        .iter_mut()
        .find(|m| m.field == field && m.from == from && m.to == to)
    {
        row.rows += 1;
        return;
    }
    mapping.push(MappingNote {
        field,
        from: from.to_string(),
        to: to.to_string(),
        rows: 1,
    });
}

/// The kbc-review/1 document. Front matter carries only what the block
/// actually said; the body states the migration's own provenance, because a
/// reader six months from now needs to know this prose was not written by a
/// reviewer looking at this diff.
fn render_document(
    summary_md: &str,
    risk: Option<&(String, String)>,
    block_id: &str,
    findings: &[DocFinding],
    skipped: &[SkippedFinding],
) -> String {
    let mut out = String::from("---\nschema: kbc-review/1\nsummary_md: |\n");
    for line in summary_md.lines() {
        out.push_str(&format!("  {line}\n"));
    }
    if let Some((level, why)) = risk {
        out.push_str("risk:\n");
        out.push_str(&format!("  level: {level}\n"));
        out.push_str(&format!("  why: {}\n", yaml_scalar(why)));
    }
    out.push_str("blocks:\n");
    out.push_str("  context: |\n");
    out.push_str(&format!(
        "    Migrated from a legacy review artifact by `kb-code review import-legacy`\n    \
         (kbc-legacy-import/1). Only the embedded <script id=\"{block_id}\"> machine block was\n    \
         read; the artifact's prose was NOT scraped (design D9-a). {} finding(s) were\n    \
         migrated{}.\n",
        findings.len(),
        if skipped.is_empty() {
            String::new()
        } else {
            format!(" and {} skipped for want of a location", skipped.len())
        }
    ));
    out.push_str("---\n\n");
    out.push_str("# Migrated review\n\n");
    out.push_str(summary_md.trim());
    out.push_str("\n\n## Provenance\n\n");
    out.push_str(
        "This document is a mechanical translation of a legacy review artifact's machine\nblock. \
         Nothing in it was re-derived from the current code: every finding below is\nthe legacy \
         reviewer's claim, carried across so it can be dispositioned, not\nre-verified. Run \
         `kb-code review lint` before composing.\n",
    );
    out
}

/// A one-line YAML scalar, quoted when it needs to be. The document is
/// parsed back by K1's deliberately-strict closed YAML subset, so an
/// unquoted `why` containing a `:` would be a parse error the operator
/// would have to fix by hand.
fn yaml_scalar(s: &str) -> String {
    let one_line = s.replace(['\n', '\r'], " ");
    if one_line.contains(':') || one_line.contains('#') || one_line.trim() != one_line {
        format!(
            "\"{}\"",
            one_line.replace('\\', "\\\\").replace('"', "\\\"")
        )
    } else {
        one_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/review-legacy/acme-app-PR-42.html");

    #[test]
    fn the_fixture_imports_and_matches_the_golden() {
        let out = import(FIXTURE, &ImportOptions::default()).expect("imports");
        let golden: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/review-legacy/acme-app-PR-42.golden.json"
        ))
        .expect("golden parses");
        let actual = serde_json::to_value(&out).expect("serialises");
        assert_eq!(
            actual,
            golden,
            "the legacy import changed shape — regenerate the golden deliberately:\n{}",
            serde_json::to_string_pretty(&actual).unwrap()
        );
    }

    #[test]
    fn an_artifact_with_no_machine_block_is_refused_by_name() {
        let err = import(
            "<html><body><h1>A review</h1><p>Looks fine.</p></body></html>",
            &ImportOptions::default(),
        )
        .unwrap_err();
        match &err {
            ImportError::NoBlock { looked_for, hint } => {
                assert_eq!(looked_for.len(), BLOCK_IDS.len());
                assert!(hint.contains("NOT scraped"));
            }
            other => panic!("expected NoBlock, got {other:?}"),
        }
        assert!(err.to_string().contains("kb-review-data"));
    }

    #[test]
    fn the_prose_is_never_read_even_when_it_looks_like_a_finding() {
        // A body full of finding-shaped prose, and a machine block that
        // declares exactly ONE finding. The importer must emit one.
        let html = r#"<html><body>
<section class="finding severity-blocker"><h3>SQL injection in OrderQuery</h3>
<p>path: app/models/order.rb line 12</p></section>
<section class="finding severity-concern"><h3>N+1 in the dashboard</h3></section>
<script type="application/json" id="kb-review-data">
{"summary":"one","findings":[{"title":"only this one","severity":"ok","path":"a.rb"}]}
</script></body></html>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].title, "only this one");
        assert!(!out.doc_md.contains("SQL injection"));
    }

    #[test]
    fn a_finding_with_no_path_is_skipped_never_invented() {
        let html = r#"<script type="application/json" id="review-data">
{"summary":"s","findings":[{"title":"no location here","severity":"concern"}]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert!(out.findings.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert_eq!(out.skipped[0].title.as_deref(), Some("no location here"));
        assert!(out.skipped[0].reason.contains("finding_no_location"));
    }

    #[test]
    fn an_unknown_severity_is_substituted_with_a_stated_note_or_refused() {
        let html = r#"<script type="application/json" id="kb-review-data">
{"summary":"s","findings":[{"title":"t","severity":"spicy","path":"a.rb"}]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.findings[0].severity, "concern");
        assert_eq!(out.mapping.len(), 1);
        assert_eq!(out.mapping[0].from, "spicy");
        assert_eq!(out.mapping[0].to, "concern");

        let strict = import(
            html,
            &ImportOptions {
                strict: true,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(strict, ImportError::Strict { .. }));
    }

    #[test]
    fn declared_severity_aliases_map_without_a_substitution_warning_being_lost() {
        let html = r#"<script type="application/json" id="kb-review-data">
{"summary":"s","findings":[{"title":"t","severity":"critical","path":"a.rb"},
                           {"title":"u","severity":"nit","path":"b.rb"}]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.findings[0].severity, "blocker");
        assert!(out.findings[0].blocking);
        assert_eq!(out.findings[0].act, "issue");
        assert_eq!(out.findings[1].severity, "ok");
        assert!(!out.findings[1].blocking);
        assert_eq!(out.findings[1].act, "note");
        // Both aliases are REPORTED, not silently applied.
        assert_eq!(out.mapping.len(), 2);
    }

    #[test]
    fn no_legacy_id_is_ever_adopted_as_a_slug() {
        let html = r#"<script type="application/json" id="kb-review-data">
{"summary":"s","findings":[{"id":"f-7","title":"t","severity":"ok","path":"a.rb"}]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.findings[0].slug, None);
        assert!(out.findings[0].rationale.contains("legacy finding `f-7`"));
    }

    #[test]
    fn a_verdict_becomes_a_risk_level_through_a_stated_rule() {
        for (verdict, level) in [
            ("request-changes", "high"),
            ("comment", "medium"),
            ("approve", "low"),
        ] {
            let html = format!(
                r#"<script type="application/json" id="kb-review-data">
{{"summary":"s","verdict":"{verdict}","findings":[]}}</script>"#
            );
            let out = import(&html, &ImportOptions::default()).unwrap();
            assert!(
                out.doc_md.contains(&format!("level: {level}")),
                "{verdict} -> {level}"
            );
            assert_eq!(out.tier, "standard");
        }
    }

    #[test]
    fn a_block_that_is_not_json_refuses_naming_the_block() {
        let html = r#"<script type="application/json" id="kb-review-data">not json</script>"#;
        match import(html, &ImportOptions::default()).unwrap_err() {
            ImportError::BlockNotJson { block_id, .. } => assert_eq!(block_id, "kb-review-data"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_empty_block_refuses_rather_than_producing_an_empty_review() {
        let html = r#"<script type="application/json" id="kb-review-data">{"pr":42}</script>"#;
        assert!(matches!(
            import(html, &ImportOptions::default()).unwrap_err(),
            ImportError::Empty { .. }
        ));
    }

    #[test]
    fn attribute_order_and_quoting_do_not_hide_the_block() {
        for attrs in [
            r#"type="application/json" id="kb-review-data""#,
            r#"id="kb-review-data" type="application/json""#,
            r#"id='kb-review-data' type='application/json'"#,
        ] {
            let html = format!(r#"<script {attrs}>{{"summary":"s","findings":[]}}</script>"#);
            assert!(extract_block(&html).is_ok(), "{attrs}");
        }
    }

    #[test]
    fn a_json_ld_script_without_a_known_id_is_not_mistaken_for_the_block() {
        let html = r#"<script type="application/json" id="analytics">{"summary":"nope"}</script>"#;
        assert!(matches!(
            extract_block(html),
            Err(ImportError::NoBlock { .. })
        ));
    }

    #[test]
    fn the_produced_document_parses_as_kbc_review_1() {
        let out = import(FIXTURE, &ImportOptions::default()).expect("imports");
        let doc = crate::review_doc::parse(&out.doc_md)
            .expect("the emitted document must parse through K1's own front-matter parser");
        assert!(!doc.summary_md.trim().is_empty());
        assert!(doc.risk.is_some());
        assert!(doc.blocks.contains_key("context"));
    }

    #[test]
    fn the_sidecar_is_exactly_the_shape_compose_accepts() {
        let out = import(FIXTURE, &ImportOptions::default()).expect("imports");
        let json = serde_json::to_string(&serde_json::json!({ "findings": out.findings })).unwrap();
        let back: serde_json::Value = serde_json::from_str(&json).unwrap();
        let parsed: Vec<DocFinding> = serde_json::from_value(back["findings"].clone()).unwrap();
        assert_eq!(parsed, out.findings);
    }
}
