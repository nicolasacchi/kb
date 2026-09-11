//! `kbc-review/1` LINT — the pure pre-flight (V73-K1, design D9's
//! "`ref resolve` and `review lint` pre-flight").
//!
//! Lint answers one question: **if I composed this document right now, what
//! would be wrong with it?** It writes nothing, and `compose --dry-run` is
//! literally this plus the card resolution — same code path, so a document
//! that lints clean and then fails to compose would be a bug in one place,
//! not a disagreement between two.
//!
//! # Severities
//!
//! | severity | meaning |
//! |---|---|
//! | `error` | `compose` REFUSES this document |
//! | `warn` | `compose` accepts it, and the author almost certainly did not mean this |
//! | `info` | a true observation the author may want, and never a reason to refuse |
//!
//! An `info` is not filler: `bare_wikilink` is how an author learns that
//! `[[Order]]` went to kb's link grammar rather than this one (root
//! invariant #29), and `stale_sha` is how they learn a citation survived
//! only because the ladder carried it.

use crate::review_doc::cards::{Card, STATE_CARRIED, STATE_ORPHAN};
use crate::review_doc::refs::{self, RefClass};
use crate::review_doc::{self, DocFinding, ReviewDoc, Tier};
use crate::store::{self, Store};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

pub const SCHEMA: &str = "review-lint/1";

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub const SEVERITY_ERROR: &str = "error";
pub const SEVERITY_WARN: &str = "warn";
pub const SEVERITY_INFO: &str = "info";

/// Every rule this lint can emit, with its fixed severity. Declared as ONE
/// table so the docs, the tests and the emitter cannot disagree about the
/// rule set — `every_emitted_rule_is_declared` walks it.
pub const RULES: &[(&str, &str)] = &[
    ("doc_parse", SEVERITY_ERROR),
    ("size_cap", SEVERITY_ERROR),
    ("tier_unmet", SEVERITY_ERROR),
    ("ref_orphan", SEVERITY_ERROR),
    ("ref_wrong_patchset", SEVERITY_ERROR),
    ("finding_no_location", SEVERITY_ERROR),
    ("finding_vocabulary", SEVERITY_ERROR),
    ("duplicate_fingerprint", SEVERITY_ERROR),
    ("duplicate_slug", SEVERITY_ERROR),
    ("ref_malformed", SEVERITY_WARN),
    ("bare_wikilink", SEVERITY_INFO),
    ("stale_sha", SEVERITY_INFO),
    ("bare_symbol_mention", SEVERITY_INFO),
    ("question_without_ref", SEVERITY_INFO),
    ("question_stale", SEVERITY_INFO),
    ("category_mapped", SEVERITY_INFO),
];

/// V73-K5 (gap 7) — a `to_agent` question with no `answers` ref is worth a
/// nudge only once it has had time to be noticed; a question raised an hour
/// ago is not "stale", it is "new". `structural_rows` degrades this rule to
/// silence (never a guess) when it is not TOLD how old the document is —
/// see that function's `composed_at` doc.
pub const QUESTION_STALE_DAYS: i64 = 14;

pub fn severity_of(rule: &str) -> &'static str {
    RULES
        .iter()
        .find(|(r, _)| *r == rule)
        .map(|(_, s)| *s)
        .unwrap_or(SEVERITY_ERROR)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LintRow {
    pub rule: &'static str,
    pub severity: &'static str,
    pub message: String,
    /// 1-based document line, when the finding has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// The ref body this row is about, when it is about one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// The nearest things the author might have meant. Never a fix that is
    /// applied for them — a suggestion, in their own vocabulary.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
}

impl LintRow {
    fn new(rule: &'static str, message: impl Into<String>) -> LintRow {
        LintRow {
            rule,
            severity: severity_of(rule),
            message: message.into(),
            line: None,
            r#ref: None,
            candidates: Vec::new(),
        }
    }
    fn at(mut self, line: u32) -> LintRow {
        self.line = Some(line);
        self
    }
    fn about(mut self, r: impl Into<String>) -> LintRow {
        self.r#ref = Some(r.into());
        self
    }
    fn with(mut self, candidates: Vec<String>) -> LintRow {
        self.candidates = candidates;
        self
    }

    /// V76-R1c — a V0 free-text category was rewritten onto the closed set.
    pub fn category_mapped(slug: &str, from: &str, to: &str) -> LintRow {
        LintRow::new(
            "category_mapped",
            format!("category {from:?} on {slug} mapped to {to:?}"),
        )
    }
}

/// The whole lint result. `errors`/`warnings`/`infos` are COUNTS of `rows`,
/// derived here so no consumer has to re-count (and so two consumers can
/// never disagree).
#[derive(Debug, Clone, Serialize)]
pub struct LintOut {
    pub schema: &'static str,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub rows: Vec<LintRow>,
}

impl LintOut {
    pub fn from_rows(mut rows: Vec<LintRow>) -> LintOut {
        // Errors first, then warnings, then infos; within a severity, by
        // document line then rule name. A stable order means two lints of
        // the same document are byte-identical.
        fn rank(s: &str) -> u8 {
            match s {
                SEVERITY_ERROR => 0,
                SEVERITY_WARN => 1,
                _ => 2,
            }
        }
        rows.sort_by(|a, b| {
            rank(a.severity)
                .cmp(&rank(b.severity))
                .then_with(|| a.line.unwrap_or(u32::MAX).cmp(&b.line.unwrap_or(u32::MAX)))
                .then_with(|| a.rule.cmp(b.rule))
                .then_with(|| a.message.cmp(&b.message))
        });
        LintOut {
            schema: SCHEMA,
            errors: rows.iter().filter(|r| r.severity == SEVERITY_ERROR).count(),
            warnings: rows.iter().filter(|r| r.severity == SEVERITY_WARN).count(),
            infos: rows.iter().filter(|r| r.severity == SEVERITY_INFO).count(),
            rows,
        }
    }

    pub fn ok(&self) -> bool {
        self.errors == 0
    }

    /// Append extra rows and re-derive the counts. Used by the V0 compose
    /// path to splice `category_mapped` INFO onto the document lint.
    pub fn with_extra(self, extra: Vec<LintRow>) -> LintOut {
        if extra.is_empty() {
            return self;
        }
        let mut rows = self.rows;
        rows.extend(extra);
        LintOut::from_rows(rows)
    }
}

/// A document that did not even parse — the one lint that can be produced
/// without a [`ReviewDoc`].
pub fn parse_failure(e: &review_doc::DocError) -> LintOut {
    let mut row = LintRow::new("doc_parse", e.message.clone());
    row.line = e.line;
    LintOut::from_rows(vec![row])
}

/// The size caps, checked before anything else because an oversized
/// document should refuse with NUMBERS rather than be half-analysed.
pub fn size_rows(doc_md: &str, ref_count: usize, finding_count: usize) -> Vec<LintRow> {
    let mut rows = Vec::new();
    if doc_md.len() > review_doc::MAX_DOC_BYTES {
        rows.push(LintRow::new(
            "size_cap",
            format!(
                "the document is {} bytes; the cap is {} — refusing rather than truncating, \
                 because a review silently missing its last paragraph is worse than one that \
                 did not land",
                doc_md.len(),
                review_doc::MAX_DOC_BYTES
            ),
        ));
    }
    if ref_count > review_doc::MAX_REFS {
        rows.push(LintRow::new(
            "size_cap",
            format!(
                "the document cites {ref_count} distinct refs; the cap is {}",
                review_doc::MAX_REFS
            ),
        ));
    }
    if finding_count > review_doc::MAX_FINDINGS {
        rows.push(LintRow::new(
            "size_cap",
            format!(
                "the compose carries {finding_count} findings; the cap is {}",
                review_doc::MAX_FINDINGS
            ),
        ));
    }
    rows
}

/// Everything that can be checked WITHOUT the repository: tiers, findings
/// vocabulary and identity, the prose scan's own observations.
///
/// `composed_at` is the STORED revision's own `created_at` (unix seconds)
/// when this is a lint of a document that already exists, and `None` for a
/// candidate that has not been composed yet (a `compose --dry-run`
/// pre-flight, or `kb-code review lint --doc <file>` on a local file). The
/// `question_stale` rule needs a real age to compare against and degrades
/// to SILENCE rather than a guess when it has none — a document with no
/// birthday cannot be "14 days old".
pub fn structural_rows(
    doc: &ReviewDoc,
    raw: &str,
    tier: Tier,
    findings: Option<&[DocFinding]>,
    composed_at: Option<i64>,
) -> Vec<LintRow> {
    let mut rows = Vec::new();

    for p in review_doc::tier_problems(doc, tier, findings.is_some()) {
        rows.push(LintRow::new(
            "tier_unmet",
            format!(
                "tier `{}` requires `{}`: {} (required by `{}`)",
                tier.as_str(),
                p.field,
                p.message,
                p.required_by
            ),
        ));
    }

    // --- the prose scan's own observations --------------------------------
    for found in refs::scan_refs(raw) {
        match found.class {
            RefClass::Malformed { body, reason } => rows.push(
                LintRow::new(
                    "ref_malformed",
                    format!("`[[{body}]]` names a kbc scheme but does not parse: {reason}"),
                )
                .at(found.line)
                .about(body),
            ),
            RefClass::Wikilink(body) if !body.trim().is_empty() => rows.push(
                LintRow::new(
                    "bare_wikilink",
                    format!(
                        "`[[{body}]]` is a kb wikilink, not a kbc ref — kb-code leaves it as text \
                         (root invariant #29). Prefix it (`ent:`, `sym:`, `code:`…) if you meant a \
                         kbc reference."
                    ),
                )
                .at(found.line)
                .about(body),
            ),
            _ => {}
        }
    }

    for (line, name) in bare_symbol_mentions(raw) {
        rows.push(
            LintRow::new(
                "bare_symbol_mention",
                format!(
                    "`{name}` reads like a symbol but is plain prose — write `[[sym:{name}]]` \
                     (or `[[ent:{name}]]`) to make it a live card"
                ),
            )
            .at(line),
        );
    }

    let stale_after = composed_at.map(|c| c + QUESTION_STALE_DAYS * 86_400);
    for q in &doc.questions {
        if q.r#ref.is_none() {
            rows.push(LintRow::new(
                "question_without_ref",
                format!(
                    "the {} question {:?} has no `ref` — a question with a location is one click \
                     to answer, one without is a search",
                    q.to,
                    truncate(&q.ask)
                ),
            ));
        }
        if q.to == "to_agent" && q.answers.is_none() {
            if let Some(threshold) = stale_after {
                if now_unix() >= threshold {
                    rows.push(LintRow::new(
                        "question_stale",
                        format!(
                            "the to_agent question {:?} has had no `answers` ref for over {} \
                             days",
                            truncate(&q.ask),
                            QUESTION_STALE_DAYS
                        ),
                    ));
                }
            }
        }
    }

    // --- findings ---------------------------------------------------------
    let Some(findings) = findings else {
        return rows;
    };
    let mut by_fingerprint: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen_slugs: HashSet<&str> = HashSet::new();
    for (i, f) in findings.iter().enumerate() {
        let label = f
            .slug
            .clone()
            .unwrap_or_else(|| format!("findings[{i}] {:?}", truncate(&f.title)));
        if let Some(slug) = f.slug.as_deref() {
            if !seen_slugs.insert(slug) {
                rows.push(LintRow::new(
                    "duplicate_slug",
                    format!("two findings in this compose both claim the slug {slug:?}"),
                ));
            }
            if !crate::review_findings::is_valid_finding_slug(slug) {
                rows.push(LintRow::new(
                    "finding_vocabulary",
                    format!("{label}: slug {slug:?} is not `f-[a-z0-9-]+`"),
                ));
            }
        }
        if !review_doc::is_valid_act(&f.act) {
            rows.push(LintRow::new(
                "finding_vocabulary",
                format!(
                    "{label}: act {:?} is not one of {}",
                    f.act,
                    review_doc::ACTS.join("|")
                ),
            ));
        }
        if !store::is_valid_severity(&f.severity) {
            rows.push(LintRow::new(
                "finding_vocabulary",
                format!(
                    "{label}: severity {:?} is not one of {}",
                    f.severity,
                    store::SEVERITIES.join("|")
                ),
            ));
        }
        if !review_doc::is_valid_category_v2(&f.category) {
            rows.push(LintRow::new(
                "finding_vocabulary",
                format!(
                    "{label}: category {:?} is not one of {}",
                    f.category,
                    review_doc::CATEGORIES.join("|")
                ),
            ));
        }
        if f.location.path.trim().is_empty()
            || !store::is_valid_location_kind(&f.location.kind)
            || (f.location.kind != store::LOCATION_KIND_WHOLE_FILE
                && f.location.lines.as_ref().is_none_or(|l| l.is_empty()))
        {
            rows.push(LintRow::new(
                "finding_no_location",
                format!(
                    "{label}: every finding needs a primary location ({{path, kind, lines}}) — \
                     that anchor is what gives it the carry-forward ladder, its thread and its \
                     GitHub export; `cites` are secondary and never a substitute"
                ),
            ));
        }
        by_fingerprint
            .entry(f.fingerprint())
            .or_default()
            .push(label);
    }
    for (fp, labels) in by_fingerprint {
        if labels.len() > 1 {
            let mut labels = labels;
            labels.sort();
            rows.push(LintRow::new(
                "duplicate_fingerprint",
                format!(
                    "{} findings share the fingerprint {fp} (same act, category, normalised \
                     title and path): {} — reconciliation could not tell them apart, so they \
                     would fight over one slug",
                    labels.len(),
                    labels.join(", ")
                ),
            ));
        }
    }
    rows
}

/// The rows that need the resolved cards — one per orphaned or carried ref.
/// Takes the SAME cards the read route returns, so lint and the reader can
/// never disagree about which refs resolved.
pub fn card_rows(store: &Store, repo_id: i64, cards: &[Card]) -> Vec<LintRow> {
    let mut rows = Vec::new();
    for c in cards {
        match c.state {
            STATE_ORPHAN => {
                let wrong_ps = c.caption.contains("has no patchset")
                    || c.caption.contains("re-read with ?ps=");
                let rule = if wrong_ps {
                    "ref_wrong_patchset"
                } else {
                    "ref_orphan"
                };
                rows.push(
                    LintRow::new(rule, c.caption.clone())
                        .about(c.r#ref.clone())
                        .with(candidates_for(store, repo_id, &c.r#ref)),
                );
            }
            STATE_CARRIED => rows.push(
                LintRow::new(
                    "stale_sha",
                    format!(
                        "{} resolved by carrying it forward, not by a blob match: {}",
                        c.r#ref, c.caption
                    ),
                )
                .about(c.r#ref.clone()),
            ),
            _ => {}
        }
    }
    rows
}

/// Up to five "did you mean" candidates for an unresolvable ref. Purely
/// additive information: nothing is ever rewritten for the author.
fn candidates_for(store: &Store, repo_id: i64, raw: &str) -> Vec<String> {
    let Ok(Some(r)) = refs::parse_ref(raw) else {
        return Vec::new();
    };
    match r {
        refs::Ref::Sym { name, .. } => match store.symbols_named_in_repo(repo_id, &name) {
            Ok(rows) => {
                let mut out: Vec<String> = rows
                    .into_iter()
                    .map(|(_, s)| match s.container {
                        Some(c) => format!("sym:{c}#{}", s.name),
                        None => format!("sym:{}", s.name),
                    })
                    .collect();
                out.sort();
                out.dedup();
                out.truncate(5);
                out
            }
            Err(_) => Vec::new(),
        },
        refs::Ref::Ent { fqn, .. } => match store.entity_defs_for_name(repo_id, None, &fqn, 5) {
            Ok(rows) => {
                let mut out: Vec<String> =
                    rows.into_iter().map(|r| format!("ent:{}", r.fqn)).collect();
                out.sort();
                out.dedup();
                out.truncate(5);
                out
            }
            Err(_) => Vec::new(),
        },
        refs::Ref::Code { path, .. } => {
            let base = path.rsplit('/').next().unwrap_or(&path).to_string();
            match store.paths_with_basename(repo_id, &base, 5) {
                Ok(paths) => paths.into_iter().map(|p| format!("code:{p}")).collect(),
                Err(_) => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// `Namespace::Class`-shaped names in prose that carry no `sym:`/`ent:`
/// prefix. The `::` is REQUIRED and a bare CapWord is never reported — the
/// exact closed grammar kb's own doc↔code bridge settled on (root invariant
/// #2), for the same reason: `Order` in a sentence is a word, `Shop::Order`
/// is an address someone forgot to make clickable.
///
/// Reported once per distinct name, at its FIRST prose line.
pub fn bare_symbol_mentions(doc: &str) -> Vec<(u32, String)> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for (lineno, text) in refs::prose_lines(doc) {
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            if !is_ident_start_upper(chars[i]) || (i > 0 && is_ident_char(chars[i - 1])) {
                i += 1;
                continue;
            }
            let start = i;
            let mut end = i;
            let mut segments = 1usize;
            loop {
                while end < chars.len() && is_ident_char(chars[end]) {
                    end += 1;
                }
                if end + 1 < chars.len()
                    && chars[end] == ':'
                    && chars[end + 1] == ':'
                    && end + 2 < chars.len()
                    && is_ident_start_upper(chars[end + 2])
                {
                    end += 2;
                    segments += 1;
                    continue;
                }
                break;
            }
            if segments >= 2 {
                let name: String = chars[start..end].iter().collect();
                if seen.insert(name.clone()) {
                    out.push((lineno, name));
                }
            }
            i = end.max(start + 1);
        }
    }
    out
}

fn is_ident_start_upper(c: char) -> bool {
    c.is_ascii_uppercase()
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn truncate(s: &str) -> String {
    let t: String = s.chars().take(60).collect();
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_doc::parse;

    const FIXTURE: &str = include_str!("../../tests/fixtures/review-doc/acme-app-standard.md");

    fn rules(rows: &[LintRow]) -> Vec<&str> {
        let mut v: Vec<&str> = rows.iter().map(|r| r.rule).collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn every_declared_rule_has_one_severity_and_no_duplicates() {
        let mut names: Vec<&str> = RULES.iter().map(|(r, _)| *r).collect();
        let n = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), n, "a rule is declared twice");
        for (_, s) in RULES {
            assert!(
                [SEVERITY_ERROR, SEVERITY_WARN, SEVERITY_INFO].contains(s),
                "unknown severity {s:?}"
            );
        }
    }

    #[test]
    fn the_fixture_lints_clean_of_errors_at_its_declared_tier() {
        let doc = parse(FIXTURE).expect("parses");
        let findings = doc.findings.clone();
        let rows = structural_rows(&doc, FIXTURE, Tier::Standard, findings.as_deref(), None);
        let errors: Vec<&LintRow> = rows
            .iter()
            .filter(|r| r.severity == SEVERITY_ERROR)
            .collect();
        assert!(errors.is_empty(), "{errors:#?}");
    }

    #[test]
    fn a_bare_wikilink_is_reported_as_information_never_as_an_error() {
        let doc = parse(FIXTURE).expect("parses");
        let rows = structural_rows(&doc, FIXTURE, Tier::Standard, doc.findings.as_deref(), None);
        let hit = rows
            .iter()
            .find(|r| r.rule == "bare_wikilink")
            .expect("the fixture contains a bare [[Order]]");
        assert_eq!(hit.severity, SEVERITY_INFO);
        assert!(hit.message.contains("kb wikilink"), "{}", hit.message);
    }

    #[test]
    fn a_malformed_ref_is_a_warning_and_names_its_reason() {
        let raw = "---\nsummary_md: x\nfindings: []\n---\nSee [[code:]] please.\n";
        let doc = parse(raw).expect("parses");
        let rows = structural_rows(&doc, raw, Tier::Minimal, doc.findings.as_deref(), None);
        let hit = rows
            .iter()
            .find(|r| r.rule == "ref_malformed")
            .expect("warned");
        assert_eq!(hit.severity, SEVERITY_WARN);
        assert_eq!(hit.line, Some(5));
        assert!(hit.message.contains("empty path"), "{}", hit.message);
    }

    #[test]
    fn two_findings_with_one_fingerprint_are_a_named_error() {
        let raw = "---\n\
                   summary_md: x\n\
                   findings:\n\
                   \x20 - act: issue\n\
                   \x20   severity: concern\n\
                   \x20   category: correctness\n\
                   \x20   title: Race on retry\n\
                   \x20   rationale: a\n\
                   \x20   location:\n\
                   \x20     path: a.rb\n\
                   \x20     kind: single\n\
                   \x20     lines: [1]\n\
                   \x20 - act: issue\n\
                   \x20   severity: blocker\n\
                   \x20   category: correctness\n\
                   \x20   title: race   on RETRY\n\
                   \x20   rationale: b\n\
                   \x20   location:\n\
                   \x20     path: a.rb\n\
                   \x20     kind: single\n\
                   \x20     lines: [9]\n\
                   ---\n";
        let doc = parse(raw).expect("parses");
        let rows = structural_rows(&doc, raw, Tier::Minimal, doc.findings.as_deref(), None);
        assert!(rules(&rows).contains(&"duplicate_fingerprint"), "{rows:#?}");
    }

    #[test]
    fn a_finding_with_no_usable_location_is_an_error() {
        let raw = "---\n\
                   summary_md: x\n\
                   findings:\n\
                   \x20 - act: issue\n\
                   \x20   severity: concern\n\
                   \x20   category: correctness\n\
                   \x20   title: t\n\
                   \x20   rationale: r\n\
                   \x20   location:\n\
                   \x20     path: a.rb\n\
                   \x20     kind: single\n\
                   ---\n";
        let doc = parse(raw).expect("parses");
        let rows = structural_rows(&doc, raw, Tier::Minimal, doc.findings.as_deref(), None);
        assert!(rules(&rows).contains(&"finding_no_location"), "{rows:#?}");
    }

    #[test]
    fn tier_problems_surface_as_errors_naming_the_tier_and_the_field() {
        let raw = "---\nsummary_md: x\nfindings: []\n---\n";
        let doc = parse(raw).expect("parses");
        let rows = structural_rows(&doc, raw, Tier::Full, doc.findings.as_deref(), None);
        let msgs: Vec<&str> = rows
            .iter()
            .filter(|r| r.rule == "tier_unmet")
            .map(|r| r.message.as_str())
            .collect();
        assert_eq!(
            msgs.len(),
            3,
            "`full` promises risk, an author block and at least one named section: {msgs:#?}"
        );
        assert!(msgs.iter().any(|m| m.contains("`risk`")), "{msgs:#?}");
        assert!(msgs.iter().any(|m| m.contains("`author`")), "{msgs:#?}");
        assert!(msgs.iter().any(|m| m.contains("`blocks`")), "{msgs:#?}");
    }

    // --- V73-K5 gap 7: an unanswered to_agent question, over time ----------

    fn doc_with_one_to_agent_question(answers: Option<&str>) -> ReviewDoc {
        let answers_line = answers
            .map(|a| format!("\n    answers: {a}"))
            .unwrap_or_default();
        let raw = format!(
            "---\nsummary_md: x\nfindings: []\nquestions:\n  - to: to_agent\n    ask: \
             is this safe?{answers_line}\n---\n"
        );
        parse(&raw).expect("parses")
    }

    #[test]
    fn an_unanswered_to_agent_question_is_silent_with_no_known_age() {
        let doc = doc_with_one_to_agent_question(None);
        let rows = structural_rows(&doc, "x", Tier::Minimal, doc.findings.as_deref(), None);
        assert!(
            !rules(&rows).contains(&"question_stale"),
            "a candidate document with no `composed_at` has no age to be stale about: {rows:#?}"
        );
    }

    #[test]
    fn an_unanswered_to_agent_question_is_silent_while_fresh() {
        let doc = doc_with_one_to_agent_question(None);
        let now = now_unix();
        let rows = structural_rows(
            &doc,
            "x",
            Tier::Minimal,
            doc.findings.as_deref(),
            Some(now - 3600), // an hour old
        );
        assert!(!rules(&rows).contains(&"question_stale"), "{rows:#?}");
    }

    #[test]
    fn an_unanswered_to_agent_question_warns_info_once_stale() {
        let doc = doc_with_one_to_agent_question(None);
        let now = now_unix();
        let rows = structural_rows(
            &doc,
            "x",
            Tier::Minimal,
            doc.findings.as_deref(),
            Some(now - (QUESTION_STALE_DAYS + 1) * 86_400),
        );
        let hit = rows
            .iter()
            .find(|r| r.rule == "question_stale")
            .expect("stale after the threshold");
        assert_eq!(hit.severity, SEVERITY_INFO);
    }

    #[test]
    fn an_answered_to_agent_question_never_goes_stale() {
        let doc = doc_with_one_to_agent_question(Some("finding:f-1"));
        let now = now_unix();
        let rows = structural_rows(
            &doc,
            "x",
            Tier::Minimal,
            doc.findings.as_deref(),
            Some(now - (QUESTION_STALE_DAYS + 30) * 86_400),
        );
        assert!(!rules(&rows).contains(&"question_stale"), "{rows:#?}");
    }

    #[test]
    fn bare_symbol_mentions_need_a_double_colon_and_skip_code() {
        let doc = "Order is a word. Shop::Order is an address.\n\
                   `Other::Thing` is code.\n\
                   ```\nFenced::Thing\n```\n\
                   Shop::Order again on another line.\n";
        let got = bare_symbol_mentions(doc);
        assert_eq!(got, vec![(1, "Shop::Order".to_string())]);
    }

    #[test]
    fn size_caps_refuse_with_numbers_rather_than_truncating() {
        let big = "x".repeat(review_doc::MAX_DOC_BYTES + 1);
        let rows = size_rows(&big, 0, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rule, "size_cap");
        assert!(
            rows[0]
                .message
                .contains(&format!("{}", review_doc::MAX_DOC_BYTES + 1)),
            "the refusal must name the actual size: {}",
            rows[0].message
        );
    }

    #[test]
    fn the_out_envelope_counts_and_orders_deterministically() {
        let out = LintOut::from_rows(vec![
            LintRow::new("bare_wikilink", "i").at(9),
            LintRow::new("tier_unmet", "e"),
            LintRow::new("ref_malformed", "w").at(2),
        ]);
        assert_eq!((out.errors, out.warnings, out.infos), (1, 1, 1));
        assert_eq!(
            out.rows.iter().map(|r| r.rule).collect::<Vec<_>>(),
            vec!["tier_unmet", "ref_malformed", "bare_wikilink"]
        );
        assert!(!out.ok());
    }
}
