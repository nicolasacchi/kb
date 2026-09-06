//! `kbc-review/1` — the review DOCUMENT model (V73-K1, design D9/D9-a).
//!
//! A review is a **Markdown document with YAML front matter**, stored per
//! review + patchset as an append-only chain of revisions (`review_docs`,
//! migration V0034). The body is Markdown and never HTML: D9-a records why
//! (HTML is a permanent XSS surface, cannot be interdiffed across
//! re-reviews, and its references are dead text). The operator's HTML
//! artifact is met one level up — [`render`] turns the SAME document into
//! HTML through the operator's own template, as an EXPORT, on request.
//!
//! # The four rules of this module
//!
//! 1. **`compose` is the one authoring transaction.** Prose + findings +
//!    report + verdict land in ONE sqlite transaction
//!    (`Store::compose_review_doc`). `findings import`, `report --set` and
//!    `verdict` remain documented low-level twins — the same writes, one
//!    at a time, each with its own commit.
//! 2. **A tier is a promise, and an omission is stated.** `minimal`
//!    promises summary + findings; `standard` adds risk and a reading
//!    order (authored, or DERIVED from the existing review map and
//!    captioned `derived`); `full` adds an author block and at least one
//!    named section. Everything a document does NOT carry is listed in
//!    `omitted[]` on every read — a reader never discovers a hole, they
//!    are told about it.
//! 3. **A ref that does not resolve is an ORPHAN, never a guessed line.**
//!    [`cards`] runs the SAME carry-forward ladder review comments use
//!    (`annotations::resolve` + `review_comments::line_matches_snippet`);
//!    an uncertain match is an honest orphan.
//! 4. **A slug is identity; a fingerprint is a change detector.** `f-<n>`
//!    slugs are minted once per review from a monotonic LEDGER
//!    (`review_finding_slugs`) and are never reused, not even after the
//!    finding they named is tombstoned. Reconciliation matches on the
//!    fingerprint and keeps the slug; a finding that vanishes from a
//!    re-compose is tombstoned, never renumbered; a `manual` finding is
//!    never superseded by a compose (V0024's origin rule, unchanged).
//!
//! # Submodules
//!
//! | module | what |
//! |---|---|
//! | [`frontmatter`] | the closed YAML-subset parser |
//! | [`refs`] | the scheme-prefixed ref grammar (golden-pinned) |
//! | [`cards`] | ref → live card, via the existing ladder |
//! | [`lint`] | the pure pre-flight |
//! | [`render`] | Markdown + cards → HTML through an operator template |
//! | [`routes`] | the three read routes + `compose`'s document half |

pub mod cards;
pub mod frontmatter;
pub mod lint;
pub mod refs;
pub mod render;
pub mod routes;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The document's own schema tag, carried in front matter AND echoed on
/// every response.
pub const SCHEMA: &str = "kbc-review/1";

/// Soft cap on the WHOLE document (front matter + body). Over it, `compose`
/// REFUSES with the byte count and the limit — never a truncation, because
/// a review silently missing its last paragraph is worse than a review that
/// did not land.
pub const MAX_DOC_BYTES: usize = 256 * 1024;

/// Soft cap on distinct refs in one document. Same refusal posture.
pub const MAX_REFS: usize = 2_000;

/// Soft cap on findings in one compose. Same refusal posture.
pub const MAX_FINDINGS: usize = 500;

// --- tiers -----------------------------------------------------------------

/// `minimal | standard | full` — what this document PROMISES. A tier is
/// priced authoring, not a quality grade: an honest `minimal` beats a
/// `full` with three empty sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Minimal,
    Standard,
    Full,
}

pub const TIERS: [&str; 3] = ["minimal", "standard", "full"];

impl Tier {
    pub fn parse(s: &str) -> Option<Tier> {
        match s {
            "minimal" => Some(Tier::Minimal),
            "standard" => Some(Tier::Standard),
            "full" => Some(Tier::Full),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Minimal => "minimal",
            Tier::Standard => "standard",
            Tier::Full => "full",
        }
    }
}

/// The OPTIONAL blocks whose absence is reported rather than assumed. The
/// order is the order `omitted[]` reports them in, so two reads of the same
/// document produce byte-identical output.
pub const OPTIONAL_BLOCKS: [&str; 5] = ["risk", "reading_order", "flows", "questions", "author"];

/// The closed set of NAMED sections `blocks:` may carry. An unknown name is
/// refused by name (`normalize_report_shape`'s precedent) rather than
/// stored as a section nothing will ever render.
pub const BLOCK_NAMES: [&str; 6] = [
    "context",
    "approach",
    "alternatives_considered",
    "tests",
    "rollout",
    "open_questions",
];

/// The closed set of top-level front-matter keys.
pub const FRONT_MATTER_KEYS: [&str; 9] = [
    "schema",
    "summary_md",
    "risk",
    "reading_order",
    "blocks",
    "findings",
    "flows",
    "questions",
    "author",
];

pub const RISK_LEVELS: [&str; 3] = ["low", "medium", "high"];

pub const QUESTION_TARGETS: [&str; 3] = ["to_author", "to_reviewer", "to_agent"];

pub const AUTHOR_KINDS: [&str; 2] = ["agent", "human"];

// --- findings v2 -----------------------------------------------------------

/// `act` — the SPEECH ACT axis D9 adds beside `severity`. An `issue` and a
/// `question` about the same line at the same severity are different things
/// to a reader and to a triage queue, and v1 could not say which was which.
pub const ACTS: [&str; 8] = [
    "issue",
    "question",
    "suggestion",
    "nitpick",
    "praise",
    "note",
    "todo",
    "chore",
];

/// `category` v2 — the SUBJECT axis. Note that `review_findings.category`
/// remains free text at the STORAGE layer (V0024's own convention, and
/// every pre-V0034 row's value): this closed vocabulary is enforced on the
/// `compose`/document path only, and `lint` reports an out-of-vocabulary
/// category on the low-level `findings import` twin as an INFO rather than
/// rewriting anyone's existing rows.
pub const CATEGORIES: [&str; 8] = [
    "correctness",
    "security",
    "performance",
    "design",
    "tests",
    "docs",
    "style",
    "other",
];

pub fn is_valid_act(s: &str) -> bool {
    ACTS.contains(&s)
}

pub fn is_valid_category_v2(s: &str) -> bool {
    CATEGORIES.contains(&s)
}

/// A finding's CHANGE DETECTOR: FNV-1a 64 over
/// `act \0 category \0 normalised(title) \0 primary path`, rendered as 16
/// hex digits.
///
/// Three deliberate choices. (a) The SLUG is not an input — the whole point
/// is that a finding keeps its slug when its wording drifts. (b) The title
/// is normalised (lowercased, whitespace collapsed, trimmed) so a reflowed
/// sentence is the same finding, while a different sentence is a different
/// one. (c) `severity` and `blocking` are NOT inputs: a reviewer raising a
/// concern to a blocker is the same finding with a changed severity, and
/// making that mint a new slug would orphan the human's disposition.
///
/// Same FNV-1a constants as `lib::salt_set_fingerprint` — this crate's
/// house hash for a short stable identifier that is not a security claim.
pub fn fingerprint(act: &str, category: &str, title: &str, path: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for field in [act, category, &normalize_title(title), path] {
        // The NUL terminator after each field is what stops
        // `("ab", "c")` and `("a", "bc")` hashing the same.
        for b in field.as_bytes().iter().chain(std::iter::once(&0u8)) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Lowercase, collapse every run of whitespace to one space, trim.
pub fn normalize_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        for lc in c.to_lowercase() {
            out.push(lc);
        }
    }
    out
}

// --- the typed document ----------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Risk {
    pub level: String,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Stop {
    /// The ref, as written.
    pub r#ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chapter {
    pub chapter: String,
    pub stops: Vec<Stop>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Flow {
    pub name: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    /// `to_author` | `to_reviewer` | `to_agent`.
    pub to: String,
    pub ask: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Author {
    /// `agent` | `human`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub considered: Vec<String>,
    pub not_considered: Vec<String>,
}

/// One finding as a DOCUMENT carries it (front matter or sidecar) — the v2
/// shape. The primary location is the same `{path, kind, lines, removed}`
/// object `kbc-findings/1` already uses, because that is what gives the
/// finding its annotation anchor and therefore its ladder, its thread and
/// its GitHub export. `cites` are SECONDARY refs and never compete with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocFinding {
    /// Optional: absent means "mint me a slug". An explicit slug is
    /// IDENTITY and is honoured (and recorded in the ledger as taken).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(default = "default_act")]
    pub act: String,
    pub severity: String,
    pub category: String,
    #[serde(default)]
    pub blocking: bool,
    pub title: String,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
    pub location: crate::review_findings::FindingLocationBody,
    #[serde(default)]
    pub cites: Vec<String>,
    /// Slugs this finding REPLACES. Written to the replaced row's
    /// `superseded_by`; never inferred (guessing which new finding "is
    /// really" an old one is the wrong-exact class the oracle bar forbids).
    #[serde(default)]
    pub supersedes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<crate::review_findings::FindingEvidenceBody>,
}

fn default_act() -> String {
    "issue".to_string()
}

impl DocFinding {
    /// This finding's change-detector fingerprint.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.act, &self.category, &self.title, &self.location.path)
    }
}

/// The parsed document. `body_md` is the prose after the front matter;
/// `raw` is the WHOLE thing, byte-for-byte, which is what gets stored.
#[derive(Debug, Clone)]
pub struct ReviewDoc {
    pub summary_md: String,
    pub risk: Option<Risk>,
    pub reading_order: Vec<Chapter>,
    pub blocks: BTreeMap<String, String>,
    pub findings: Option<Vec<DocFinding>>,
    pub flows: Vec<Flow>,
    pub questions: Vec<Question>,
    pub author: Option<Author>,
    pub body_md: String,
    pub body_line: u32,
}

/// A parse refusal, with the front-matter line when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocError {
    pub line: Option<u32>,
    pub message: String,
}

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(n) => write!(f, "line {n}: {}", self.message),
            None => write!(f, "{}", self.message),
        }
    }
}

impl DocError {
    fn msg(message: impl Into<String>) -> Self {
        Self {
            line: None,
            message: message.into(),
        }
    }
}

impl From<frontmatter::FrontMatterError> for DocError {
    fn from(e: frontmatter::FrontMatterError) -> Self {
        Self {
            line: Some(e.line),
            message: e.message,
        }
    }
}

/// Parse a `kbc-review/1` document. Structural only: whether the document
/// satisfies its TIER is [`tier_problems`]'s question, and whether its refs
/// resolve is [`cards`]'s.
pub fn parse(doc: &str) -> Result<ReviewDoc, DocError> {
    let split = frontmatter::split(doc)?;
    let obj = split
        .front
        .as_object()
        .ok_or_else(|| DocError::msg("front matter must be a mapping"))?;

    if let Some(bad) = obj
        .keys()
        .find(|k| !FRONT_MATTER_KEYS.contains(&k.as_str()))
    {
        return Err(DocError::msg(format!(
            "unknown front-matter key {bad:?} (accepted: {})",
            FRONT_MATTER_KEYS.join(", ")
        )));
    }
    if let Some(s) = obj.get("schema") {
        let s = s
            .as_str()
            .ok_or_else(|| DocError::msg("`schema` must be a string"))?;
        if s != SCHEMA {
            return Err(DocError::msg(format!(
                "unsupported schema {s:?} (expected {SCHEMA:?})"
            )));
        }
    }

    let summary_md = match obj.get("summary_md") {
        Some(Value::String(s)) => s.clone(),
        Some(_) => return Err(DocError::msg("`summary_md` must be a string")),
        None => String::new(),
    };

    let risk = match obj.get("risk") {
        None | Some(Value::Null) => None,
        Some(Value::Object(m)) => {
            let level = m
                .get("level")
                .and_then(Value::as_str)
                .ok_or_else(|| DocError::msg("`risk.level` must be a string"))?;
            if !RISK_LEVELS.contains(&level) {
                return Err(DocError::msg(format!(
                    "`risk.level` must be one of {}, got {level:?}",
                    RISK_LEVELS.join("|")
                )));
            }
            let why = m
                .get("why")
                .and_then(Value::as_str)
                .ok_or_else(|| DocError::msg("`risk.why` must be a one-line string"))?;
            Some(Risk {
                level: level.to_string(),
                why: why.to_string(),
            })
        }
        Some(_) => return Err(DocError::msg("`risk` must be a mapping {level, why}")),
    };

    let reading_order = match obj.get("reading_order") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let m = it
                    .as_object()
                    .ok_or_else(|| DocError::msg("each reading_order entry is {chapter, stops}"))?;
                let chapter = m
                    .get("chapter")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DocError::msg("`reading_order[].chapter` must be a string"))?;
                let stops = match m.get("stops") {
                    Some(Value::Array(ss)) => {
                        let mut v = Vec::with_capacity(ss.len());
                        for s in ss {
                            v.push(parse_stop(s)?);
                        }
                        v
                    }
                    None | Some(Value::Null) => Vec::new(),
                    Some(_) => {
                        return Err(DocError::msg("`reading_order[].stops` must be a sequence"))
                    }
                };
                out.push(Chapter {
                    chapter: chapter.to_string(),
                    stops,
                });
            }
            out
        }
        Some(_) => return Err(DocError::msg("`reading_order` must be a sequence")),
    };

    let mut blocks = BTreeMap::new();
    match obj.get("blocks") {
        None | Some(Value::Null) => {}
        Some(Value::Object(m)) => {
            for (k, v) in m {
                if !BLOCK_NAMES.contains(&k.as_str()) {
                    return Err(DocError::msg(format!(
                        "unknown block {k:?} (accepted: {})",
                        BLOCK_NAMES.join(", ")
                    )));
                }
                let s = v.as_str().ok_or_else(|| {
                    DocError::msg(format!("block {k:?} must be a Markdown string"))
                })?;
                blocks.insert(k.clone(), s.to_string());
            }
        }
        Some(_) => {
            return Err(DocError::msg(
                "`blocks` must be a mapping of named sections",
            ))
        }
    }

    let findings = match obj.get("findings") {
        None => None,
        Some(Value::Null) => Some(Vec::new()),
        Some(v @ Value::Array(_)) => Some(
            serde_json::from_value::<Vec<DocFinding>>(v.clone()).map_err(|e| {
                DocError::msg(format!(
                    "`findings` is not a kbc-review/1 v2 finding list: {e}"
                ))
            })?,
        ),
        Some(_) => return Err(DocError::msg("`findings` must be a sequence")),
    };

    let flows = match obj.get("flows") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let m = it
                    .as_object()
                    .ok_or_else(|| DocError::msg("each flow is {name, steps}"))?;
                let name = m
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DocError::msg("`flows[].name` must be a string"))?;
                let steps = string_list(m.get("steps"), "flows[].steps")?;
                out.push(Flow {
                    name: name.to_string(),
                    steps,
                });
            }
            out
        }
        Some(_) => return Err(DocError::msg("`flows` must be a sequence")),
    };

    let questions = match obj.get("questions") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let m = it
                    .as_object()
                    .ok_or_else(|| DocError::msg("each question is {to, ask, ref?}"))?;
                let to = m
                    .get("to")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DocError::msg("`questions[].to` must be a string"))?;
                if !QUESTION_TARGETS.contains(&to) {
                    return Err(DocError::msg(format!(
                        "`questions[].to` must be one of {}, got {to:?}",
                        QUESTION_TARGETS.join("|")
                    )));
                }
                let ask = m
                    .get("ask")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DocError::msg("`questions[].ask` must be a string"))?;
                let r = match m.get("ref") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => return Err(DocError::msg("`questions[].ref` must be a string")),
                };
                out.push(Question {
                    to: to.to_string(),
                    ask: ask.to_string(),
                    r#ref: r,
                });
            }
            out
        }
        Some(_) => return Err(DocError::msg("`questions` must be a sequence")),
    };

    let author = match obj.get("author") {
        None | Some(Value::Null) => None,
        Some(Value::Object(m)) => {
            let kind = m
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| DocError::msg("`author.kind` must be a string"))?;
            if !AUTHOR_KINDS.contains(&kind) {
                return Err(DocError::msg(format!(
                    "`author.kind` must be one of {}, got {kind:?}",
                    AUTHOR_KINDS.join("|")
                )));
            }
            Some(Author {
                kind: kind.to_string(),
                model: m.get("model").and_then(Value::as_str).map(str::to_string),
                session_id: m
                    .get("session_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                considered: string_list(m.get("considered"), "author.considered")?,
                not_considered: string_list(m.get("not_considered"), "author.not_considered")?,
            })
        }
        Some(_) => return Err(DocError::msg("`author` must be a mapping")),
    };

    Ok(ReviewDoc {
        summary_md,
        risk,
        reading_order,
        blocks,
        findings,
        flows,
        questions,
        author,
        body_md: split.body.to_string(),
        body_line: split.body_line,
    })
}

fn parse_stop(v: &Value) -> Result<Stop, DocError> {
    match v {
        Value::String(s) => Ok(Stop {
            r#ref: s.clone(),
            why: None,
        }),
        Value::Object(m) => {
            let r = m
                .get("ref")
                .and_then(Value::as_str)
                .ok_or_else(|| DocError::msg("a reading-order stop needs a `ref`"))?;
            Ok(Stop {
                r#ref: r.to_string(),
                why: m.get("why").and_then(Value::as_str).map(str::to_string),
            })
        }
        _ => Err(DocError::msg(
            "a reading-order stop is a ref string or {ref, why}",
        )),
    }
}

fn string_list(v: Option<&Value>, what: &str) -> Result<Vec<String>, DocError> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| DocError::msg(format!("`{what}` must be a list of strings")))
            })
            .collect(),
        Some(_) => Err(DocError::msg(format!("`{what}` must be a sequence"))),
    }
}

// --- tiers + omissions -----------------------------------------------------

/// One unmet tier requirement, as `lint` and `compose` both report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TierProblem {
    /// The front-matter field that is missing or empty.
    pub field: String,
    /// The tier that requires it.
    pub required_by: &'static str,
    pub message: String,
}

/// Which of this document's tier requirements are unmet.
///
/// The reading of D9's sentence, ratified here because the design states it
/// two ways: "required core = summary + findings, every other block
/// optional with a stated omission degrade" AND "full adds blocks/flows/
/// questions/author". Both are honoured by making the TIER gate require
/// only what a tier can promise WITHOUT a substitute — `reading_order` has
/// a deterministic substitute (the review map, captioned `derived`) so it
/// is never a tier failure, while `flows` and `questions` have no
/// substitute at all and are therefore reported as OMISSIONS at every tier
/// rather than as failures at `full`. `author` and at least one named
/// `blocks` section ARE required at `full`: they are the accountability and
/// the substance a `full` review claims to have done.
pub fn tier_problems(doc: &ReviewDoc, tier: Tier, findings_supplied: bool) -> Vec<TierProblem> {
    let mut out = Vec::new();
    if doc.summary_md.trim().is_empty() {
        out.push(TierProblem {
            field: "summary_md".to_string(),
            required_by: "minimal",
            message: "every tier requires a non-empty `summary_md`".to_string(),
        });
    }
    if !findings_supplied {
        out.push(TierProblem {
            field: "findings".to_string(),
            required_by: "minimal",
            message: "supply `findings` (front matter or a sidecar) — an EMPTY list is a valid \
                      and explicit claim that this review found nothing, which is different \
                      from not having looked"
                .to_string(),
        });
    }
    if tier >= Tier::Standard && doc.risk.is_none() {
        out.push(TierProblem {
            field: "risk".to_string(),
            required_by: "standard",
            message: "`standard` promises a risk level (low|medium|high) and a one-line why"
                .to_string(),
        });
    }
    if tier >= Tier::Full {
        if doc.author.is_none() {
            out.push(TierProblem {
                field: "author".to_string(),
                required_by: "full",
                message: "`full` promises an author block ({kind, model?, session_id?, \
                          considered, not_considered})"
                    .to_string(),
            });
        }
        if doc.blocks.is_empty() {
            out.push(TierProblem {
                field: "blocks".to_string(),
                required_by: "full",
                message: format!(
                    "`full` promises at least one named section ({})",
                    BLOCK_NAMES.join(", ")
                ),
            });
        }
    }
    out
}

/// The optional blocks this document does NOT carry, in [`OPTIONAL_BLOCKS`]
/// order. Reported on every read so an absence is stated, never discovered.
pub fn omitted_blocks(doc: &ReviewDoc) -> Vec<String> {
    let mut out = Vec::new();
    for name in OPTIONAL_BLOCKS {
        let present = match name {
            "risk" => doc.risk.is_some(),
            "reading_order" => !doc.reading_order.is_empty(),
            "flows" => !doc.flows.is_empty(),
            "questions" => !doc.questions.is_empty(),
            "author" => doc.author.is_some(),
            _ => true,
        };
        if !present {
            out.push(name.to_string());
        }
    }
    for name in BLOCK_NAMES {
        if !doc.blocks.contains_key(name) {
            out.push(format!("blocks.{name}"));
        }
    }
    out
}

/// Every ref this document mentions, from BOTH surfaces — the prose scan
/// and the typed front-matter fields — deduplicated by `raw`, prose first
/// then front matter, each in document order. One list, so a card is built
/// once no matter how many places cite it.
pub fn all_refs(doc: &ReviewDoc) -> Vec<refs::Ref> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<refs::Ref> = Vec::new();

    fn push_scan(
        text: &str,
        out: &mut Vec<refs::Ref>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        for r in refs::refs_in(text) {
            if seen.insert(r.raw().to_string()) {
                out.push(r);
            }
        }
    }
    fn push_typed(
        raw: &str,
        out: &mut Vec<refs::Ref>,
        seen: &mut std::collections::HashSet<String>,
    ) {
        if let Ok(Some(r)) = refs::parse_ref(raw) {
            if seen.insert(r.raw().to_string()) {
                out.push(r);
            }
        }
    }

    push_scan(&doc.body_md, &mut out, &mut seen);
    push_scan(&doc.summary_md, &mut out, &mut seen);
    for b in doc.blocks.values() {
        push_scan(b, &mut out, &mut seen);
    }
    for ch in &doc.reading_order {
        for s in &ch.stops {
            push_typed(&s.r#ref, &mut out, &mut seen);
        }
    }
    for f in &doc.flows {
        for s in &f.steps {
            push_typed(s, &mut out, &mut seen);
        }
    }
    for q in &doc.questions {
        if let Some(r) = &q.r#ref {
            push_typed(r, &mut out, &mut seen);
        }
    }
    if let Some(fs) = &doc.findings {
        for f in fs {
            for c in &f.cites {
                push_typed(c, &mut out, &mut seen);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/review-doc/acme-app-standard.md");

    #[test]
    fn the_synthetic_fixture_parses_and_satisfies_its_declared_tier() {
        let doc = parse(FIXTURE).expect("fixture parses");
        assert!(!doc.summary_md.trim().is_empty());
        assert_eq!(doc.risk.as_ref().map(|r| r.level.as_str()), Some("medium"));
        assert_eq!(doc.reading_order.len(), 2);
        assert!(doc.findings.is_some());
        assert!(
            tier_problems(&doc, Tier::Standard, doc.findings.is_some()).is_empty(),
            "the fixture claims `standard`"
        );
    }

    #[test]
    fn a_full_tier_document_missing_its_author_block_is_a_named_tier_problem() {
        let doc = parse(FIXTURE).expect("parses");
        let problems = tier_problems(&doc, Tier::Full, true);
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert!(fields.contains(&"author"), "{problems:?}");
    }

    #[test]
    fn findings_absent_entirely_is_a_tier_problem_but_an_empty_list_is_not() {
        let doc = parse("---\nsummary_md: ok\n---\nbody\n").expect("parses");
        assert!(doc.findings.is_none());
        let p = tier_problems(&doc, Tier::Minimal, false);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].field, "findings");

        let doc = parse("---\nsummary_md: ok\nfindings: []\n---\nbody\n").expect("parses");
        assert_eq!(doc.findings.as_ref().map(Vec::len), Some(0));
        assert!(tier_problems(&doc, Tier::Minimal, true).is_empty());
    }

    #[test]
    fn an_unknown_front_matter_key_or_block_is_refused_by_name() {
        let e = parse("---\nsummary_md: x\nnope: 1\n---\n").expect_err("refused");
        assert!(e.message.contains("nope"), "{e}");
        let e = parse("---\nsummary_md: x\nblocks:\n  nope: y\n---\n").expect_err("refused");
        assert!(e.message.contains("nope"), "{e}");
    }

    #[test]
    fn omissions_are_listed_in_a_stable_order() {
        let doc = parse("---\nsummary_md: x\nfindings: []\n---\n").expect("parses");
        assert_eq!(
            omitted_blocks(&doc),
            vec![
                "risk",
                "reading_order",
                "flows",
                "questions",
                "author",
                "blocks.context",
                "blocks.approach",
                "blocks.alternatives_considered",
                "blocks.tests",
                "blocks.rollout",
                "blocks.open_questions",
            ]
        );
    }

    #[test]
    fn the_fingerprint_ignores_severity_and_wording_drift_but_not_meaning() {
        let base = fingerprint("issue", "correctness", "Dedup race on retry", "app/x.rb");
        assert_eq!(
            base,
            fingerprint(
                "issue",
                "correctness",
                "  dedup   RACE on retry ",
                "app/x.rb"
            ),
            "reflowing/casing a title is the same finding"
        );
        assert_ne!(
            base,
            fingerprint("question", "correctness", "Dedup race on retry", "app/x.rb")
        );
        assert_ne!(
            base,
            fingerprint("issue", "security", "Dedup race on retry", "app/x.rb")
        );
        assert_ne!(
            base,
            fingerprint("issue", "correctness", "Dedup race on refund", "app/x.rb")
        );
        assert_ne!(
            base,
            fingerprint("issue", "correctness", "Dedup race on retry", "app/y.rb")
        );
        assert_eq!(base.len(), 16);
    }

    #[test]
    fn all_refs_collects_from_prose_and_from_typed_fields_without_duplicates() {
        let doc = parse(FIXTURE).expect("parses");
        let got: Vec<String> = all_refs(&doc).iter().map(|r| r.raw().to_string()).collect();
        let mut sorted = got.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), got.len(), "all_refs must not repeat a ref");
        assert!(got.iter().any(|r| r.starts_with("code:")), "{got:?}");
        assert!(got.iter().any(|r| r.starts_with("finding:")), "{got:?}");
    }

    #[test]
    fn every_declared_vocabulary_is_non_empty_and_unique() {
        for (name, v) in [
            ("ACTS", ACTS.to_vec()),
            ("CATEGORIES", CATEGORIES.to_vec()),
            ("TIERS", TIERS.to_vec()),
            ("BLOCK_NAMES", BLOCK_NAMES.to_vec()),
            ("FRONT_MATTER_KEYS", FRONT_MATTER_KEYS.to_vec()),
            ("RISK_LEVELS", RISK_LEVELS.to_vec()),
            ("QUESTION_TARGETS", QUESTION_TARGETS.to_vec()),
            ("AUTHOR_KINDS", AUTHOR_KINDS.to_vec()),
            ("OPTIONAL_BLOCKS", OPTIONAL_BLOCKS.to_vec()),
        ] {
            let mut s = v.clone();
            s.sort_unstable();
            s.dedup();
            assert_eq!(s.len(), v.len(), "{name} has a duplicate entry");
            assert!(!v.is_empty(), "{name} is empty");
        }
        // Every `Tier` string round-trips through `parse`.
        for t in TIERS {
            assert_eq!(Tier::parse(t).map(Tier::as_str), Some(t));
        }
    }
}
