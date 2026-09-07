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

use crate::review_doc::{self, DocFinding};
use crate::review_findings::{FindingEvidenceBody, FindingLocationBody};
use crate::store;
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
    // V73-K5 — kb-code's OWN pre-v7.3 findings-ledger schema
    // (`kbc-findings/1`, the PR Room's `findings import` route). An
    // artifact carrying kb-code's own former schema under its own former
    // id was, before this, invisible to the importer.
    "kbc-findings",
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
/// Per-finding FLAT cited-code-excerpt sources — a legacy schema's own
/// scraped snippet text, as opposed to the TYPED `{lang, source}` shape
/// `evidence` (below) already names. Never trusted as a still-true fact
/// about the file (it may be stale text quoted at review time): carried
/// forward as a live `cites` ref instead — see [`map_finding`]'s doc.
pub const EXCERPT_KEYS: &[&str] = &["excerpt", "code_excerpt", "snippet", "code"];

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
        let gt_rel = html[open_start..].find('>')?;
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

/// One finding's location, from EITHER shape this importer accepts: the
/// generic flat `PATH_KEYS` string (with `line`/`lines` top-level
/// siblings), or the SAME nested `{path, kind, lines}` object
/// [`DocFinding`] and `kbc-findings/1` already use elsewhere in this crate.
/// Returns `(path, lines, declared_kind)` — `declared_kind` is `Some` only
/// for the nested shape, and only when THAT object actually names one.
fn location_fields(raw: &Value) -> Option<(String, Option<Vec<i64>>, Option<String>)> {
    if let Some(path) = first_str(raw, PATH_KEYS) {
        let lines = raw
            .get("line")
            .and_then(Value::as_i64)
            .map(|l| vec![l])
            .or_else(|| {
                raw.get("lines")
                    .and_then(|l| l.as_array())
                    .map(|a| a.iter().filter_map(Value::as_i64).collect::<Vec<i64>>())
            })
            .filter(|v| !v.is_empty());
        return Some((path.to_string(), lines, None));
    }
    let obj = raw.get("location")?.as_object()?;
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let lines = obj
        .get("lines")
        .and_then(|l| l.as_array())
        .map(|a| a.iter().filter_map(Value::as_i64).collect::<Vec<i64>>())
        .filter(|v| !v.is_empty());
    let kind = obj.get("kind").and_then(Value::as_str).map(str::to_string);
    Some((path.to_string(), lines, kind))
}

/// `single | range | multi | whole_file` from a line count — the SAME rule
/// `kbc-findings/1`'s own writers use (`review_findings::validate_location_
/// shape`'s vocabulary). Only used for the NESTED shape's default when that
/// object names no `kind` of its own, or names one outside the closed set;
/// the flat-shape derivation elsewhere in this file is unchanged (a
/// pre-existing, separately-tracked defect — see this unit's report).
fn kind_from_lines(lines: Option<&[i64]>) -> &'static str {
    match lines.map(|l| l.len()).unwrap_or(0) {
        0 => store::LOCATION_KIND_WHOLE_FILE,
        1 => store::LOCATION_KIND_SINGLE,
        2 => store::LOCATION_KIND_RANGE,
        _ => store::LOCATION_KIND_MULTI,
    }
}

/// The finding's cited code excerpt, when the raw JSON carries one — EITHER
/// the typed `{lang, source}` shape `kbc-findings/1`'s own `evidence` field
/// already uses, or a flat scraped snippet string under one of
/// [`EXCERPT_KEYS`].
enum RawExcerpt {
    /// Carried through verbatim as findings-v2 `evidence` — a typed,
    /// author-attributed excerpt, not a guess.
    Typed(FindingEvidenceBody),
    /// A bare scraped string. NEVER copied verbatim into a field that
    /// looks authoritative (it may be stale the moment the legacy artifact
    /// was rendered) — [`map_finding`] turns this into a live `cites` ref
    /// instead, so a reader always sees CURRENT bytes, never frozen ones.
    Flat(String),
}

fn raw_excerpt(raw: &Value) -> Option<RawExcerpt> {
    if let Some(obj) = raw.get("evidence").and_then(Value::as_object) {
        let lang = obj.get("lang").and_then(Value::as_str).map(str::to_string);
        let source = obj
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string);
        if lang.is_some() || source.is_some() {
            return Some(RawExcerpt::Typed(FindingEvidenceBody { lang, source }));
        }
    }
    first_str(raw, EXCERPT_KEYS).map(|s| RawExcerpt::Flat(s.to_string()))
}

/// A `code:` ref citing `path`/`lines` — no `@sha`: this importer has no
/// repository to read a blob hash from (D9-a's "local, no daemon, no
/// network" verb), so the honest ref is unpinned, resolving live against
/// whatever the target patchset reads today.
fn code_cite(path: &str, lines: Option<&[i64]>) -> String {
    match lines {
        None | Some([]) => format!("code:{path}"),
        Some([one]) => format!("code:{path}:{one}"),
        Some(many) => {
            let lo = many.iter().min().copied().unwrap_or(0);
            let hi = many.iter().max().copied().unwrap_or(0);
            format!("code:{path}:{lo}-{hi}")
        }
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
    let excerpt = raw_excerpt(raw);
    let Some((path, lines, declared_kind)) = location_fields(raw) else {
        let excerpt_note = if excerpt.is_some() {
            " (this finding also cited a code excerpt, which could not be carried forward \
             without a location either)"
        } else {
            ""
        };
        return Ok(Err(format!(
            "no location path (looked for {PATH_KEYS:?}, including the nested {{path, kind, \
             lines}} shape) — a finding with no location fails kbc-review/1's own \
             `finding_no_location` lint, and inventing one would be worse than omitting \
             it{excerpt_note}"
        )));
    };

    let raw_sev = first_str(raw, &["severity", "level", "impact"]).unwrap_or("concern");
    let severity = map_severity(raw_sev, opts, mapping)?;
    let raw_cat = first_str(raw, &["category", "kind", "type"]);
    let category = map_category(raw_cat, opts, mapping)?;

    // `act` is a v2 axis the legacy shape has no column for — UNLESS the
    // raw finding already IS a v2 shape (this daemon's own export, or a
    // hand-authored kbc-findings/1 block), in which case its own `act` is
    // honoured rather than re-derived. Absent or out-of-vocabulary, the
    // existing stated rule applies: an `ok` row is a note, everything else
    // an issue.
    let act = first_str(raw, &["act"])
        .filter(|a| review_doc::is_valid_act(a))
        .map(str::to_string)
        .unwrap_or_else(|| {
            if severity == "ok" {
                "note".to_string()
            } else {
                "issue".to_string()
            }
        });
    // `blocking` likewise: an explicit bool is honoured (round-trip
    // fidelity for this daemon's own export); otherwise derived from
    // severity, as before.
    let blocking = raw
        .get("blocking")
        .and_then(Value::as_bool)
        .unwrap_or(severity == "blocker");

    let derived_kind = kind_from_lines(lines.as_deref()).to_string();
    let kind = match declared_kind {
        Some(k) if store::is_valid_location_kind(&k) => k,
        Some(bad) => {
            // The nested object named a `kind` outside the closed
            // single|range|multi|whole_file vocabulary — substituted with
            // a stated note, the same tolerant-in-one-direction posture
            // `map_severity`/`map_category` already apply.
            note_mapping(mapping, "location.kind", &bad, &derived_kind);
            derived_kind
        }
        None => derived_kind,
    };

    let (cites, evidence, flat_excerpt_bytes) = match excerpt {
        Some(RawExcerpt::Typed(body)) => (raw_cites(raw), Some(body), None),
        Some(RawExcerpt::Flat(text)) => {
            let mut cites = raw_cites(raw);
            let cite = code_cite(&path, lines.as_deref());
            if !cites.contains(&cite) {
                cites.push(cite);
            }
            (cites, None, Some(text.len()))
        }
        None => (raw_cites(raw), None, None),
    };
    if let Some(bytes) = flat_excerpt_bytes {
        // The excerpt's TEXT is never quoted here — only its size, so the
        // mapping row stays an honest audit trail ("something was scraped
        // and rerouted") without itself becoming a second, un-refreshed
        // copy of possibly-stale content.
        note_mapping(
            mapping,
            "cites",
            &format!("a scraped {bytes}-byte code excerpt"),
            "a live code: ref (never the frozen excerpt text)",
        );
    }

    Ok(Ok(DocFinding {
        // No slug: the ledger mints one. Carrying a legacy id across would
        // claim an identity in a namespace that never minted it (D9's slug
        // rule) — the legacy id is preserved in the rationale instead.
        slug: None,
        act,
        severity: severity.to_string(),
        category: category.to_string(),
        blocking,
        title: title.to_string(),
        rationale: rationale_with_provenance(raw),
        recommendation: first_str(raw, RECOMMENDATION_KEYS).map(str::to_string),
        location: FindingLocationBody {
            path,
            kind,
            lines,
            removed: false,
        },
        cites,
        supersedes: raw_string_list(raw, "supersedes"),
        evidence,
    }))
}

/// `cites`/`supersedes` — a JSON array of strings, carried through
/// verbatim when the raw finding already has one (this daemon's own
/// export, or a hand-authored kbc-findings/1 block). Absent or malformed
/// degrades to empty rather than a refusal: a legacy artifact's OWN schema
/// never had these fields, so their absence is the overwhelmingly common
/// and entirely expected case.
fn raw_string_list(raw: &Value, key: &str) -> Vec<String> {
    raw.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn raw_cites(raw: &Value) -> Vec<String> {
    raw_string_list(raw, "cites")
}

/// The rationale, plus the legacy id when the block carried one — so a
/// migrated finding can still be traced back to the artifact it came from
/// without claiming that id as a kbc slug.
fn rationale_with_provenance(raw: &Value) -> String {
    let base = first_str(raw, RATIONALE_KEYS).unwrap_or("").to_string();
    match first_str(raw, &["id", "slug", "ref"]) {
        Some(id) => {
            if base.is_empty() {
                format!("(migrated from a legacy finding — legacy_id: {id})")
            } else {
                format!("{base}\n\n(migrated from a legacy finding — legacy_id: {id})")
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

// --- V73-K5: the export half — closing the round trip ----------------------
//
// `render` embeds exactly this shape under the `kbc-review` block id (one
// of [`BLOCK_IDS`]), so `import` can read a kb-code export back — the
// round trip design D9-a always intended but K1 never wired up. Nothing
// here is a second schema: it emits the SAME generic finding shape
// [`map_finding`] already reads (flat top-level scalars, the nested
// `{path, kind, lines, removed}` location, and a typed `evidence` object),
// so a fix to the importer's tolerance is automatically a fix to the
// round trip too.

/// One composed, non-superseded finding, in the shape [`map_finding`]
/// accepts. Slugs are exported (so the rationale can trace back to them
/// via `legacy_id`) but are NEVER re-adopted on import — D9's "a legacy id
/// is never a slug" rule applies to this daemon's own former output
/// exactly as it does to a stranger's.
pub fn export_finding_json(f: &store::ReviewFindingRow) -> Value {
    let lines: Option<Vec<i64>> = f
        .location_lines
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok());
    let evidence = if f.evidence_lang.is_some() || f.evidence_source.is_some() {
        serde_json::json!({ "lang": f.evidence_lang, "source": f.evidence_source })
    } else {
        Value::Null
    };
    let cites: Vec<String> = f
        .cites_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    serde_json::json!({
        "slug": f.slug,
        "act": f.act,
        "severity": f.severity,
        "category": f.category,
        "blocking": f.blocking,
        "title": f.title,
        "rationale": f.rationale,
        "recommendation": f.recommendation,
        "location": {
            "path": f.location_path,
            "kind": f.location_kind,
            "lines": lines,
            "removed": f.location_removed,
        },
        "cites": cites,
        "evidence": evidence,
    })
}

/// The whole machine block payload — front matter's own `summary_md`/
/// `risk` plus every LIVE (non-superseded) finding. Tombstoned findings
/// are deliberately excluded: re-importing an export should not resurrect
/// a finding a human already dispositioned away.
pub fn export_block_json(
    summary_md: &str,
    risk: Option<&review_doc::Risk>,
    findings: &[store::ReviewFindingRow],
) -> Value {
    let live: Vec<Value> = findings
        .iter()
        .filter(|f| !f.superseded)
        .map(export_finding_json)
        .collect();
    serde_json::json!({
        "schema": SCHEMA_KBC_REVIEW,
        "summary_md": summary_md,
        "risk": risk.map(|r| serde_json::json!({ "level": r.level, "why": r.why })),
        "findings": live,
    })
}

/// `kbc-review/1`'s OWN schema tag, as `import`'s `risk_from`/`first_str`
/// probes expect to see it — named here rather than importing
/// `review_doc::SCHEMA` under a second name, so a reader sees at a glance
/// this is the review document schema, not `kbc-legacy-import/1`'s own.
const SCHEMA_KBC_REVIEW: &str = review_doc::SCHEMA;

/// Wrap `payload` in the `<script type="application/json" id="kbc-review">`
/// block [`BLOCK_IDS`] already accepts, escaping every `</` so a rationale
/// or title that happens to contain `</script>` can never truncate the
/// block early when [`scan_script`] re-extracts it — the standard
/// embedded-JSON escape (`\/` is a legal JSON escape for `/`, so this is
/// invisible to any JSON parser, including [`import`]'s own).
pub fn export_block_html(payload: &Value) -> String {
    let json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
    let escaped = json.replace("</", "<\\/");
    format!("<script type=\"application/json\" id=\"kbc-review\">{escaped}</script>")
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
        assert!(out.findings[0].rationale.contains("legacy_id: f-7"));
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

    // --- V73-K5 gap 1: a cited code excerpt is never silently dropped ------

    #[test]
    fn a_flat_scraped_excerpt_is_carried_as_a_live_code_ref_never_as_frozen_text() {
        let html = r#"<script type="application/json" id="kb-review-data">
{"summary":"s","findings":[{"title":"t","severity":"concern","path":"a.rb","line":12,
                             "excerpt":"def totally_stale\n  1 + 1\nend"}]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.findings.len(), 1);
        // The excerpt TEXT is never reproduced verbatim anywhere in the
        // output — only a live pointer is.
        assert!(!out.doc_md.contains("totally_stale"));
        assert!(
            !out.findings[0].rationale.contains("totally_stale"),
            "{:?}",
            out.findings[0].rationale
        );
        assert_eq!(out.findings[0].cites, vec!["code:a.rb:12".to_string()]);
        assert_eq!(out.findings[0].evidence, None);
        assert!(out.mapping.iter().any(|m| m.field == "cites"));
    }

    #[test]
    fn an_excerpt_with_no_location_is_skipped_and_the_reason_says_so() {
        let html = r#"<script type="application/json" id="kb-review-data">
{"summary":"s","findings":[{"title":"nowhere","severity":"concern",
                             "excerpt":"orphaned snippet"}]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert!(out.findings.is_empty());
        assert_eq!(out.skipped.len(), 1);
        assert!(
            out.skipped[0].reason.contains("also cited a code excerpt"),
            "{}",
            out.skipped[0].reason
        );
    }

    // --- V73-K5 gap 2: the nested {path, kind, lines} location shape -------

    #[test]
    fn a_nested_location_object_is_read_the_same_as_a_flat_path() {
        let html = r#"<script type="application/json" id="kb-review-data">
{"summary":"s","findings":[{"title":"t","severity":"concern","category":"correctness",
    "rationale":"r","location":{"path":"app/models/order.rb","kind":"range","lines":[10,12]}}]}
</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.findings.len(), 1, "{:?}", out.skipped);
        let f = &out.findings[0];
        assert_eq!(f.location.path, "app/models/order.rb");
        assert_eq!(f.location.kind, "range");
        assert_eq!(f.location.lines, Some(vec![10, 12]));
    }

    #[test]
    fn a_nested_location_missing_kind_derives_one_from_the_line_count() {
        let html = r#"<script type="application/json" id="kb-review-data">
{"summary":"s","findings":[
  {"title":"a","severity":"ok","location":{"path":"a.rb","lines":[3]}},
  {"title":"b","severity":"ok","location":{"path":"b.rb","lines":[3,9,11]}},
  {"title":"c","severity":"ok","location":{"path":"c.rb"}}
]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.findings.len(), 3, "{:?}", out.skipped);
        assert_eq!(out.findings[0].location.kind, "single");
        assert_eq!(out.findings[1].location.kind, "multi");
        assert_eq!(out.findings[2].location.kind, "whole_file");
    }

    #[test]
    fn a_nested_location_with_an_out_of_vocabulary_kind_is_substituted_with_a_note() {
        let html = r#"<script type="application/json" id="kb-review-data">
{"summary":"s","findings":[{"title":"t","severity":"ok",
    "location":{"path":"a.rb","kind":"lines","lines":[3]}}]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.findings[0].location.kind, "single");
        assert!(
            out.mapping
                .iter()
                .any(|m| m.field == "location.kind" && m.from == "lines"),
            "{:?}",
            out.mapping
        );
    }

    // --- V73-K5 gap 3: kb-code's own pre-v7.3 kbc-findings/1 block --------

    #[test]
    fn a_kbc_findings_block_is_accepted_severity_preserved_slug_kept_as_legacy_id() {
        let html = r#"<script type="application/json" id="kbc-findings">
{"schema":"kbc-findings/1","findings":[{"slug":"f-3","severity":"blocker",
    "category":"correctness","title":"Dedup race","rationale":"a race on retry",
    "location":{"path":"app/x.rb","kind":"single","lines":[10]},
    "evidence":{"lang":"ruby","source":"@count += 1"}}]}</script>"#;
        let out = import(html, &ImportOptions::default()).unwrap();
        assert_eq!(out.block_id, "kbc-findings");
        assert_eq!(out.findings.len(), 1, "{:?}", out.skipped);
        let f = &out.findings[0];
        // Severity is ALREADY the kbc vocabulary — preserved, zero
        // substitution note.
        assert_eq!(f.severity, "blocker");
        assert!(out.mapping.is_empty(), "{:?}", out.mapping);
        // The slug is NEVER adopted as identity...
        assert_eq!(f.slug, None);
        // ...but is kept, traceable, as `legacy_id`.
        assert!(f.rationale.contains("legacy_id: f-3"), "{}", f.rationale);
        assert_eq!(f.location.path, "app/x.rb");
        assert_eq!(f.location.lines, Some(vec![10]));
        // The typed evidence carries through verbatim — it is NOT dropped
        // and NOT rerouted through `cites` (that rerouting is only for a
        // FLAT scraped excerpt string, never a typed evidence object).
        assert_eq!(
            f.evidence,
            Some(crate::review_findings::FindingEvidenceBody {
                lang: Some("ruby".to_string()),
                source: Some("@count += 1".to_string()),
            })
        );
    }

    // --- V73-K5 gap 4: render's machine block round-trips through import --

    #[test]
    fn export_then_import_reproduces_the_document_modulo_minted_slugs() {
        let finding = sample_finding_row();
        let payload = export_block_json(
            "Money moves from floats to integer cents.",
            Some(&review_doc::Risk {
                level: "high".to_string(),
                why: "touches billing".to_string(),
            }),
            std::slice::from_ref(&finding),
        );
        let html = format!(
            "<html><body><h1>Review</h1>{}</body></html>",
            export_block_html(&payload)
        );

        let out = import(&html, &ImportOptions::default()).expect("re-imports");
        assert_eq!(out.block_id, "kbc-review");

        let doc = crate::review_doc::parse(&out.doc_md).expect("re-imported doc parses");
        assert_eq!(
            doc.summary_md.trim_end(),
            "Money moves from floats to integer cents."
        );
        assert_eq!(doc.risk.as_ref().map(|r| r.level.as_str()), Some("high"));
        assert_eq!(
            doc.risk.as_ref().map(|r| r.why.as_str()),
            Some("touches billing")
        );

        assert_eq!(out.findings.len(), 1);
        let f = &out.findings[0];
        // Slugs are NEVER re-adopted — the one place this round trip is
        // deliberately lossy, and the report says so by name.
        assert_eq!(f.slug, None);
        assert!(f.rationale.contains("legacy_id: f-9"), "{}", f.rationale);
        assert!(f.rationale.starts_with(&finding.rationale));
        assert_eq!(f.act, finding.act);
        assert_eq!(f.severity, finding.severity);
        assert_eq!(f.category, finding.category);
        assert_eq!(f.blocking, finding.blocking);
        assert_eq!(f.title, finding.title);
        assert_eq!(f.recommendation, finding.recommendation);
        assert_eq!(f.location.path, finding.location_path);
        assert_eq!(f.location.kind, finding.location_kind);
        assert_eq!(
            f.evidence,
            Some(crate::review_findings::FindingEvidenceBody {
                lang: finding.evidence_lang.clone(),
                source: finding.evidence_source.clone(),
            })
        );
        // Zero substitutions: every value the export wrote was already
        // valid kbc vocabulary.
        assert!(out.mapping.is_empty(), "{:?}", out.mapping);
        assert!(out.skipped.is_empty(), "{:?}", out.skipped);
    }

    #[test]
    fn the_export_escapes_a_closing_script_tag_inside_a_rationale() {
        let mut finding = sample_finding_row();
        finding.rationale = "see </script><script>alert(1)</script> above".to_string();
        let payload = export_block_json("s", None, std::slice::from_ref(&finding));
        let html = format!("<html><body>{}</body></html>", export_block_html(&payload));
        // The embedded block must not have been truncated by the payload's
        // own content — re-extracting and re-parsing it must still work.
        let out = import(&html, &ImportOptions::default()).expect("re-imports despite the payload");
        assert_eq!(out.findings.len(), 1);
        assert!(out.findings[0].rationale.contains("alert(1)"));
        assert!(!html.contains("<script>alert(1)</script>"));
    }

    /// A hand-built [`crate::store::ReviewFindingRow`] — this crate has no
    /// lighter builder for one outside a live store, so the literal is
    /// spelled out in full rather than half-constructed through `Default`
    /// (this row type derives none).
    fn sample_finding_row() -> store::ReviewFindingRow {
        store::ReviewFindingRow {
            id: 1,
            review_id: 7,
            annotation_id: "ann_1".to_string(),
            slug: "f-9".to_string(),
            severity: "blocker".to_string(),
            category: "correctness".to_string(),
            location_kind: "single".to_string(),
            location_path: "app/models/order.rb".to_string(),
            location_lines: Some("[14]".to_string()),
            location_removed: false,
            title: "The backfill rounds before it multiplies".to_string(),
            rationale: "multiplies a rounded float, off by a cent".to_string(),
            recommendation: Some("Multiply first, then round".to_string()),
            evidence_lang: Some("ruby".to_string()),
            evidence_source: Some("(price * 100).round".to_string()),
            origin: "import".to_string(),
            author: None,
            disposition: None,
            disposition_note: None,
            disposition_by: None,
            disposition_at: None,
            content_updated_at: None,
            published_state: "unpublished".to_string(),
            published_at: None,
            published_url: None,
            superseded: false,
            superseded_at: None,
            superseded_reason: None,
            import_batch_id: "batch_1".to_string(),
            created_at: 0,
            updated_at: 0,
            act: "issue".to_string(),
            blocking: true,
            cites_json: None,
            fingerprint: None,
            superseded_by: None,
        }
    }
}
