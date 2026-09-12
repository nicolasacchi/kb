//! `GET /api/reviews/{id}/timeline` — ONE ordered stream of typed events for
//! a review.
//!
//! PRR-R4 shipped this as `review-timeline/1`: a pure composition of rows
//! this crate already persists for other reasons, with no storage of its
//! own. **V73-K3 widens it to `review-timeline/2`** and the widening is
//! ADDITIVE in the two ways that matter: every `review-timeline/1` event
//! keeps its `at` field and its own payload keys byte-for-byte
//! ([`tests::v1_payload_keys_are_byte_identical_under_v2`] pins that), and
//! the top-level `events` array is still there. What is new is (a) five
//! more lanes, (b) a typed envelope on every event
//! (`ts`/`author`/`ref`/`body_md`/`drift`), and (c) filters, true totals
//! and paging.
//!
//! # Why one stream
//!
//! Before this unit a reviewer had to read six surfaces to reconstruct one
//! conversation: `/timeline` for the lifecycle, `/comments` for the
//! review-scoped threads, `/github-threads` for what was said on GitHub,
//! `/doc` for what the agent composed, `/report` for its verdict, and
//! nothing at all for the PR description or an agent's claims. D9's own
//! wording is "PR description and all GitHub comments absorbed into one
//! timeline and chapter zero". A timeline that omits half the conversation
//! is not a timeline; it is a changelog.
//!
//! # The lanes
//!
//! | lane | kinds | source |
//! |---|---|---|
//! | `lifecycle` | `review_created` `pr_bound` `patchset` | `reviews`, its PR binding, its patchsets |
//! | `pr_body` | `pr_body` | the `pr_meta_json` snapshot |
//! | `findings` | `findings_import` `finding_added` `disposition` `finding_published` | `review_findings` |
//! | `verdict` | `verdict` `verdict_published` | `reviews` |
//! | `comments` | `comment` | review-scoped `annotations` |
//! | `wt_comments` | `wt_comment` | working-tree `annotations` on this review's own files |
//! | `document` | `doc_revision` | `review_docs` (K1's append-only chain) |
//! | `report` | `report` | `reviews.report_json` |
//! | `claims` | `claim` | `claims` (V0035, kbc-claim/1) |
//! | `github` | `github_comment` | LIVE, via the same `list_pull_comments` call `/github-threads` makes |
//! | `turns` | `turn` | the hunk↔turn join, `?hunk=` only, LOOPBACK only |
//!
//! Every lane reports its own state in `sources[]` — `ok`, `skipped` (the
//! caller turned it off), `refused` (the caller is not entitled to it) or
//! `degraded` (it tried and could not) with a reason. A lane that failed is
//! never silently empty; that is the v6.0 One-Inbox per-lane precedent,
//! and it is why a GitHub outage cannot make a timeline quietly lie about
//! what was said.
//!
//! Two lanes are not on by default, each for a stated reason:
//!
//! * **`github`** is a LIVE network call, which the other ten lanes are
//!   not. It is on for a PR-bound review and off otherwise, and `?github=0`
//!   turns it off; a failure degrades the lane, never the request (the
//!   `/github-threads` posture, unchanged). Thread NESTING stays on
//!   `/github-threads` — a timeline is chronological by definition, so
//!   re-parenting replies here would be a second, disagreeing answer.
//! * **`turns`** requires `?hunk=<kbc-hunkid/1>` and LOOPBACK, because the
//!   join reads raw transcript content (D19's `raw-transcript` sensitivity
//!   class ⇒ loopback-only, root invariant #4's ethos). Off loopback the
//!   lane is `refused` with that reason, never absent.
//!
//! # Ordering, totals and paging
//!
//! Events are built lane by lane in a fixed order and then STABLE-sorted by
//! `ts` ascending, so two events sharing a timestamp keep their
//! construction order — deterministic without a second tiebreak key
//! (unchanged from v1). `total` is the count AFTER filtering and BEFORE
//! paging, so a page can say what it is a page of.
//!
//! # Nothing is stored
//!
//! Unchanged, and now load-bearing for five more lanes: re-composing the
//! same review at the same state yields the same sequence, and no lane
//! writes a row, bumps a generation or emits an event.

use crate::claims;
use crate::review_pseudo;
use crate::reviews::require_review;
use crate::routes::ApiError;
use crate::state::SharedState;
use crate::store;
use crate::store::StoreBlocking;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub const SCHEMA: &str = "review-timeline/2";

/// Author names this daemon reads as an AGENT. A NAME convention and
/// nothing more — kb-code authenticates nobody, and root CLAUDE.md's
/// "identity is attribution, not authorization" ruling applies here
/// unchanged. The set is kb's own closed harness vocabulary
/// (`kb_core::sessions::HARNESSES`) plus the generic `agent`, pinned to it
/// by [`tests::the_agent_author_set_tracks_kbs_harness_vocabulary`] so the
/// two cannot drift.
pub const AGENT_AUTHOR_NAMES: &[&str] = &[
    "claude", "codex", "opencode", "grok", "kimi", "omp", "agent",
];

pub const AUTHOR_HUMAN: &str = "human";
pub const AUTHOR_AGENT: &str = "agent";
/// The daemon itself — a patchset capture has no author, and saying
/// `human` would be a small lie repeated on every review.
pub const AUTHOR_SYSTEM: &str = "system";

/// Every lane name, in composition order. Also the closed vocabulary
/// `sources[]` reports and `?lane=` would filter on if it ever grows one.
pub const LANES: &[&str] = &[
    "lifecycle",
    "pr_body",
    "findings",
    "verdict",
    "comments",
    "wt_comments",
    "document",
    "report",
    "claims",
    "github",
    "turns",
];

/// Every event kind, in no particular order — the closed set `?kind=`
/// validates against, so a typo is a 400 naming the vocabulary rather than
/// an empty page.
pub const KINDS: &[&str] = &[
    "review_created",
    "pr_bound",
    "patchset",
    "pr_body",
    "findings_import",
    "finding_added",
    "disposition",
    "verdict",
    "finding_published",
    "verdict_published",
    "comment",
    "wt_comment",
    "doc_revision",
    "report",
    "claim",
    "github_comment",
    "turn",
];

pub const DEFAULT_LIMIT: usize = 500;
pub const MAX_LIMIT: usize = 2_000;
/// How many claims the claims lane will pull for one review.
pub const MAX_CLAIMS: usize = 500;

// --- the typed event -------------------------------------------------------

/// Who produced an event. `kind` is derived from the author NAME through
/// [`author_for`]; `model`/`session_id` are carried only when the source
/// row actually recorded them, never inferred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventAuthor {
    /// `human` | `agent` | `system`.
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl EventAuthor {
    pub fn system() -> Self {
        Self {
            kind: AUTHOR_SYSTEM,
            name: None,
            model: None,
            session_id: None,
        }
    }
}

/// The author for a recorded NAME. `None` ⇒ the daemon itself.
pub fn author_for(name: Option<&str>) -> EventAuthor {
    match name {
        None => EventAuthor::system(),
        Some(n) => {
            let lower = n.trim().to_ascii_lowercase();
            EventAuthor {
                kind: if AGENT_AUTHOR_NAMES.contains(&lower.as_str()) {
                    AUTHOR_AGENT
                } else {
                    AUTHOR_HUMAN
                },
                name: Some(n.to_string()),
                model: None,
                session_id: None,
            }
        }
    }
}

/// Something the event's own subject has moved on from. Never a guess: a
/// lane emits one only when it can name both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Drift {
    pub kind: &'static str,
    pub note: String,
}

/// One event. The `detail` map is FLATTENED, which is what keeps every
/// `review-timeline/1` payload key exactly where it was.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineEvent {
    /// v1's field name, unchanged.
    pub at: i64,
    /// v2's name for the same number. Both are emitted on purpose: `at`
    /// keeps every v1 reader working, `ts` is the name the rest of v7 uses.
    pub ts: i64,
    pub kind: &'static str,
    /// Which lane produced it — so a reader can group without a second
    /// table mapping kinds to lanes.
    pub lane: &'static str,
    pub author: EventAuthor,
    /// A kbc-review/1 ref (K1's grammar) when the event HAS a location.
    /// Absent, never fabricated, when it does not.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// The event's own prose, when it has any. Markdown, verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_md: Option<String>,
    /// V76-B3 (kbc-prose/1) — `body_md`'s prose refs, attached by the route
    /// for the returned page only (never computed for filtered-out events,
    /// never persisted). Absent when the event has no body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_refs: Option<crate::prose_refs::FieldRefs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift: Option<Drift>,
    #[serde(flatten)]
    pub detail: serde_json::Map<String, serde_json::Value>,
}

impl TimelineEvent {
    fn new(at: i64, kind: &'static str, lane: &'static str, author: EventAuthor) -> Self {
        Self {
            at,
            ts: at,
            kind,
            lane,
            author,
            r#ref: None,
            body_md: None,
            body_refs: None,
            drift: None,
            detail: serde_json::Map::new(),
        }
    }

    fn with(mut self, key: &str, value: serde_json::Value) -> Self {
        self.detail.insert(key.to_string(), value);
        self
    }

    fn with_ref(mut self, r: impl Into<String>) -> Self {
        self.r#ref = Some(r.into());
        self
    }

    fn with_body(mut self, body: impl Into<String>) -> Self {
        let b = body.into();
        if !b.is_empty() {
            self.body_md = Some(b);
        }
        self
    }

    fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

/// `code:<path>[:<line>]` — K1's own grammar, built in one place so no
/// lane invents a second spelling.
fn code_ref(path: &str, line: Option<i64>) -> String {
    match line {
        Some(l) if l > 0 => format!("code:{path}:{l}"),
        _ => format!("code:{path}"),
    }
}

// --- the v1 lanes (byte-identical payloads) --------------------------------

/// The `review-timeline/1` composition, unchanged in what it emits per
/// event beyond the shared envelope. Kept as its own function — and still
/// taking only already-fetched rows — so it stays unit-testable with fixed
/// fixtures and no daemon at all.
pub(crate) fn core_events(
    review: &store::ReviewRow,
    binding: &store::ReviewPrBinding,
    patchsets: &[store::ReviewPatchsetRow],
    findings: &[store::ReviewFindingRow],
    annotations: &[store::AnnotationRow],
    verdict_published: Option<(Option<i64>, Option<String>)>,
) -> Vec<TimelineEvent> {
    let mut events: Vec<TimelineEvent> = Vec::new();

    events.push(
        TimelineEvent::new(
            review.created_at,
            "review_created",
            "lifecycle",
            EventAuthor::system(),
        )
        .with("review_id", serde_json::json!(review.id))
        .with("repo", serde_json::json!(review.repo)),
    );

    if let Some(fetched_at) = binding.pr_meta_fetched_at {
        events.push(
            TimelineEvent::new(fetched_at, "pr_bound", "lifecycle", EventAuthor::system())
                .with("pr_number", serde_json::json!(binding.pr_number))
                .with("pr_repo_slug", serde_json::json!(binding.pr_repo_slug)),
        );
    }

    for ps in patchsets {
        events.push(
            TimelineEvent::new(
                ps.captured_at,
                "patchset",
                "lifecycle",
                EventAuthor::system(),
            )
            .with("ps_number", serde_json::json!(ps.ps_number))
            .with("tip_sha", serde_json::json!(ps.tip_sha)),
        );
    }

    // Findings — `origin="import"` rows are grouped by `import_batch_id`;
    // `origin="manual"` rows are NOT, because every manual finding carries
    // the same literal `import_batch_id = "manual"` and grouping on it
    // would collapse a review's whole manual history into one fake batch
    // timestamped at whichever row a `BTreeMap` happened to keep. A
    // deliberate, documented deviation from the original brief's literal
    // wording, unchanged since PRR-R4.
    let mut import_batches: BTreeMap<&str, Vec<&store::ReviewFindingRow>> = BTreeMap::new();
    for f in findings {
        if f.origin == store::FINDING_ORIGIN_MANUAL {
            events.push(
                TimelineEvent::new(
                    f.created_at,
                    "finding_added",
                    "findings",
                    author_for(f.author.as_deref()),
                )
                .with("slug", serde_json::json!(f.slug))
                .with("title", serde_json::json!(f.title))
                .with_ref(format!("finding:{}", f.slug))
                .with_body(f.rationale.clone()),
            );
        } else {
            import_batches
                .entry(f.import_batch_id.as_str())
                .or_default()
                .push(f);
        }
    }
    for (batch_id, batch_findings) in &import_batches {
        let at = batch_findings
            .iter()
            .map(|f| f.created_at)
            .min()
            .unwrap_or(0);
        let mut slugs: Vec<&str> = batch_findings.iter().map(|f| f.slug.as_str()).collect();
        slugs.sort_unstable();
        let author = batch_findings
            .iter()
            .find_map(|f| f.author.as_deref())
            .map(|a| author_for(Some(a)))
            .unwrap_or_else(EventAuthor::system);
        events.push(
            TimelineEvent::new(at, "findings_import", "findings", author)
                .with("import_batch_id", serde_json::json!(batch_id))
                .with("count", serde_json::json!(batch_findings.len()))
                .with("slugs", serde_json::json!(slugs)),
        );
    }

    for f in findings {
        if let Some(at) = f.disposition_at {
            events.push(
                TimelineEvent::new(
                    at,
                    "disposition",
                    "findings",
                    author_for(f.disposition_by.as_deref()),
                )
                .with("slug", serde_json::json!(f.slug))
                .with("state", serde_json::json!(f.disposition))
                .with("by", serde_json::json!(f.disposition_by))
                .with_ref(format!("finding:{}", f.slug))
                .with_body(f.disposition_note.clone().unwrap_or_default()),
            );
        }
        if let Some(at) = f.published_at {
            events.push(
                TimelineEvent::new(at, "finding_published", "findings", EventAuthor::system())
                    .with("slug", serde_json::json!(f.slug))
                    .with("url", serde_json::json!(f.published_url))
                    .with_ref(format!("finding:{}", f.slug)),
            );
        }
    }

    if let Some(at) = review.verdict_at {
        events.push(
            TimelineEvent::new(at, "verdict", "verdict", EventAuthor::system())
                .with("state", serde_json::json!(review.verdict))
                .with_body(review.verdict_note.clone().unwrap_or_default()),
        );
    }

    if let Some((Some(at), url)) = verdict_published {
        events.push(
            TimelineEvent::new(at, "verdict_published", "verdict", EventAuthor::system())
                .with("url", serde_json::json!(url)),
        );
    }

    // Comment/reply activity — every review-scoped annotation that is NOT
    // itself a finding's top-level row (findings already got their own
    // lifecycle event above); a REPLY on a finding's thread DOES count —
    // that is the Q&A conversation.
    let finding_annotation_ids: HashSet<&str> =
        findings.iter().map(|f| f.annotation_id.as_str()).collect();
    for a in annotations {
        let is_finding_top_level =
            a.parent_id.is_none() && finding_annotation_ids.contains(a.id.as_str());
        if is_finding_top_level {
            continue;
        }
        let mut e = TimelineEvent::new(
            a.created_at,
            "comment",
            "comments",
            author_for(Some(&a.author)),
        )
        .with("annotation_id", serde_json::json!(a.id))
        .with("path", serde_json::json!(a.path))
        .with("intent", serde_json::json!(a.intent))
        // V73-K2c fix: this used to ALSO `.with("author", serde_json::json!(a.author))`
        // — a flat string under the SAME "author" key the struct's own
        // typed `author: EventAuthor` field (just above, from
        // `author_for(Some(&a.author))`) already serializes as. Because
        // `detail` is declared AFTER `author` in the struct and is
        // `#[serde(flatten)]`, `TimelineEvent::to_value` silently let the
        // flat string win, clobbering the v2 envelope's own structured
        // author for every `comment` event (confirmed via a standalone
        // serde repro: `to_value` on a struct shaped like this one drops
        // the typed field and keeps only the flattened duplicate). The raw
        // name is not lost — `author_for` already carries it as
        // `author.name` — so this line is deleted rather than renamed.
        .with("is_reply", serde_json::json!(a.parent_id.is_some()))
        .with_body(a.body.clone());
        if !a.path.is_empty() {
            e = e.with_ref(code_ref(&a.path, None));
        }
        events.push(e);
    }

    events
}

/// The `review-timeline/1` composition, unchanged in signature and in what
/// a v1 reader sees: [`core_events`] sorted and serialised.
///
/// `#[cfg(test)]` and honestly so — the ROUTE is what a v1 consumer talks
/// to, and it composes `core_events` itself. This function's only job now
/// is to be the thing the v1 goldens and
/// [`v2_tests::v1_payload_keys_are_byte_identical_under_v2`] measure
/// against, which is exactly why it must not quietly drift into a second
/// production path.
#[cfg(test)]
pub(crate) fn compose_review_timeline(
    review: &store::ReviewRow,
    binding: &store::ReviewPrBinding,
    patchsets: &[store::ReviewPatchsetRow],
    findings: &[store::ReviewFindingRow],
    annotations: &[store::AnnotationRow],
    verdict_published: Option<(Option<i64>, Option<String>)>,
) -> Vec<serde_json::Value> {
    let mut events = core_events(
        review,
        binding,
        patchsets,
        findings,
        annotations,
        verdict_published,
    );
    events.sort_by_key(|e| e.ts);
    events.iter().map(TimelineEvent::to_value).collect()
}

// --- the K3 lanes ----------------------------------------------------------

/// `pr_body` — ONE event, at the snapshot's own fetch time.
///
/// There is deliberately no revision chain: `pr_meta_json` is
/// wholesale-replaced by `review sweep`, so this daemon keeps exactly one
/// copy of the description and cannot honestly report what an earlier one
/// said. What it CAN report is drift — when the snapshot's head sha is not
/// the newest patchset's tip, the body may describe a different diff than
/// the one under review — and it says so rather than implying freshness.
pub(crate) fn pr_body_event(
    binding: &store::ReviewPrBinding,
    latest_tip: Option<&str>,
    pseudo: &review_pseudo::PseudoFile,
) -> Option<TimelineEvent> {
    let at = binding.pr_meta_fetched_at?;
    if !pseudo.present {
        return None;
    }
    let author = serde_json::from_str::<serde_json::Value>(binding.pr_meta_json.as_deref()?)
        .ok()
        .and_then(|v| v.get("author").and_then(|a| a.as_str()).map(str::to_string));
    let drift = match (binding.pr_head_sha.as_deref(), latest_tip) {
        (Some(head), Some(tip)) if head != tip => Some(Drift {
            kind: "pr_head",
            note: format!(
                "this description was snapshotted at PR head {head}, and the newest patchset is \
                 {tip} — it may describe a different change than the diff under review; refresh \
                 with `kb-code review sweep`"
            ),
        }),
        _ => None,
    };
    let mut e = TimelineEvent::new(at, "pr_body", "pr_body", author_for(author.as_deref()))
        .with("pr_number", serde_json::json!(binding.pr_number))
        .with("path", serde_json::json!(pseudo.path))
        .with("blob_sha", serde_json::json!(pseudo.blob_sha))
        .with("lines", serde_json::json!(pseudo.lines))
        .with_ref(format!("code:{}", pseudo.path))
        .with_body(pseudo.content().to_string());
    e.drift = drift;
    Some(e)
}

/// `doc_revision` — K1's append-only compose chain, one event per revision.
pub(crate) fn doc_revision_events(docs: &[store::ReviewDocRow]) -> Vec<TimelineEvent> {
    docs.iter()
        .map(|d| {
            let author = d
                .author_json
                .as_deref()
                .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok());
            let mut a = author_for(
                author
                    .as_ref()
                    .and_then(|v| v.get("kind").and_then(|k| k.as_str())),
            );
            if let Some(v) = &author {
                a.model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
                a.session_id = v
                    .get("session_id")
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
            }
            TimelineEvent::new(d.created_at, "doc_revision", "document", a)
                .with("revision", serde_json::json!(d.revision))
                .with("ps_number", serde_json::json!(d.ps_number))
                .with("tier", serde_json::json!(d.tier))
                .with("byte_len", serde_json::json!(d.byte_len))
                .with(
                    "omitted",
                    serde_json::from_str::<serde_json::Value>(&d.omitted_json)
                        .unwrap_or(serde_json::json!([])),
                )
                .with_ref(format!(
                    "code:{}",
                    review_pseudo::path_for(review_pseudo::REVIEW_MD)
                ))
                .with_body(d.summary_md.clone())
        })
        .collect()
}

/// `report` — the agent-authored review report. Wholesale-replaced by
/// `PUT /report`, so there is one, at its own `report_updated_at`.
pub(crate) fn report_event(report: &store::ReviewReport) -> Option<TimelineEvent> {
    let at = report.report_updated_at?;
    let json: serde_json::Value = serde_json::from_str(report.report_json.as_deref()?).ok()?;
    let mut author = author_for(
        json.get("author")
            .and_then(|a| a.as_str())
            .or(Some("claude")),
    );
    author.model = json
        .get("model")
        .and_then(|m| m.as_str())
        .map(str::to_string);
    author.session_id = json
        .get("session_id")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    Some(
        TimelineEvent::new(at, "report", "report", author)
            .with(
                "verdict",
                json.get("verdict")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            )
            .with_body(
                json.get("summary")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
    )
}

/// `wt_comment` — a WORKING-TREE annotation (no `review_id`) on a file this
/// review changes. These are the notes a reader left while reading the
/// code, not while reading the review; they belong on one stream because
/// the reviewer wrote both, and they are a DIFFERENT kind so nobody
/// mistakes one for a review comment.
pub(crate) fn wt_comment_events(annotations: &[store::AnnotationRow]) -> Vec<TimelineEvent> {
    annotations
        .iter()
        .filter(|a| a.review_id.is_none())
        .map(|a| {
            TimelineEvent::new(
                a.created_at,
                "wt_comment",
                "wt_comments",
                author_for(Some(&a.author)),
            )
            .with("annotation_id", serde_json::json!(a.id))
            .with("path", serde_json::json!(a.path))
            .with("intent", serde_json::json!(a.intent))
            // V73-K2c fix — same collision `core_events`'s comment loop had
            // (see its own comment): drop the duplicate flat "author"
            // string, since `author_for` above already carries it as
            // `author.name` on the struct's own typed field.
            .with("resolved", serde_json::json!(a.resolved))
            .with("is_reply", serde_json::json!(a.parent_id.is_some()))
            .with_ref(code_ref(&a.path, None))
            .with_body(a.body.clone())
        })
        .collect()
}

/// `claim` — kbc-claim/1 rows scoped to this review, each carrying its own
/// per-request Ladder state. SURFACED, NEVER SCORED: a claim is rendered
/// in the stream beside the fact it is about and contributes nothing to
/// any ordering beyond its own timestamp.
pub(crate) fn claim_events(
    rows: &[store::ClaimRow],
    current_blobs: &BTreeMap<String, String>,
) -> Vec<TimelineEvent> {
    rows.iter()
        .map(|c| {
            let current = c.subject_path.as_deref().and_then(|p| current_blobs.get(p));
            let (state, caption) =
                claims::ladder_state(c.blob_sha.as_deref(), current.map(String::as_str));
            let mut author = author_for(Some(c.model.as_deref().unwrap_or("agent")));
            author.model = c.model.clone();
            author.session_id = c.session_id.clone();
            let mut e = TimelineEvent::new(c.created_at, "claim", "claims", author)
                .with("claim_id", serde_json::json!(c.id))
                .with("claim_kind", serde_json::json!(c.kind))
                .with("subject_kind", serde_json::json!(c.subject_kind))
                .with("subject", serde_json::json!(c.subject))
                .with("state", serde_json::json!(state))
                .with("confidence", serde_json::json!(c.confidence))
                .with_body(c.body_md.clone());
            if let Some(p) = &c.subject_path {
                e = e.with_ref(code_ref(p, None));
            }
            if state == claims::STATE_DRIFTED {
                e.drift = Some(Drift {
                    kind: "blob",
                    note: caption,
                });
            }
            e
        })
        .collect()
}

/// `github_comment` — one event per LIVE GitHub review comment, in the
/// order GitHub reports. No thread nesting (see the module doc).
pub(crate) fn github_comment_events(
    comments: &[crate::github::PrCommentOut],
) -> Vec<TimelineEvent> {
    comments
        .iter()
        .map(|c| {
            let at = chrono::DateTime::parse_from_rfc3339(&c.created_at)
                .map(|d| d.timestamp())
                .unwrap_or(0);
            let mut e =
                TimelineEvent::new(at, "github_comment", "github", author_for(Some(&c.author)))
                    .with("comment_id", serde_json::json!(c.id))
                    .with("path", serde_json::json!(c.path))
                    .with("line", serde_json::json!(c.line.or(c.original_line)))
                    .with("side", serde_json::json!(c.side))
                    .with("html_url", serde_json::json!(c.html_url))
                    .with("is_reply", serde_json::json!(c.in_reply_to.is_some()))
                    .with_body(c.body.clone());
            e = match c.id {
                Some(id) => e.with_ref(format!("gh:comment/{id}")),
                None => e,
            };
            if let Some(p) = &c.path {
                e = e.with(
                    "code_ref",
                    serde_json::json!(code_ref(p, c.line.or(c.original_line).map(|l| l as i64))),
                );
            }
            e
        })
        .collect()
}

/// `turn` — the hunk↔turn join's matches, as timeline events. Carries the
/// bearer-visible half only (session id, `t-<uuid12>` turn id, tool, path,
/// tier); the transcript prose never reaches this stream, which is why the
/// lane is loopback-gated anyway.
pub(crate) fn turn_events(turns: &[crate::review_turns::TurnMatch]) -> Vec<TimelineEvent> {
    turns
        .iter()
        .map(|t| {
            let mut author = author_for(Some("agent"));
            author.session_id = Some(t.session_id.clone());
            TimelineEvent::new(t.ts / 1000, "turn", "turns", author)
                .with("turn_id", serde_json::json!(t.turn_id))
                .with("session_id", serde_json::json!(t.session_id))
                .with("tool", serde_json::json!(t.tool))
                .with("path", serde_json::json!(t.path))
                .with("tier", serde_json::json!(t.tier))
                .with("why", serde_json::json!(t.why))
                .with("commit", serde_json::json!(t.commit))
                .with("kb_read", serde_json::json!(t.kb_read))
                .with_ref(code_ref(&t.path, None))
        })
        .collect()
}

// --- lane reporting --------------------------------------------------------

pub const LANE_OK: &str = "ok";
pub const LANE_SKIPPED: &str = "skipped";
pub const LANE_REFUSED: &str = "refused";
pub const LANE_DEGRADED: &str = "degraded";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LaneStatus {
    pub lane: &'static str,
    pub state: &'static str,
    /// Events this lane contributed BEFORE filtering — so a filter that
    /// hides a lane cannot be mistaken for a lane that produced nothing.
    pub count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl LaneStatus {
    fn ok(lane: &'static str, count: usize) -> Self {
        Self {
            lane,
            state: LANE_OK,
            count,
            reason: None,
        }
    }
    fn not_ok(lane: &'static str, state: &'static str, reason: impl Into<String>) -> Self {
        Self {
            lane,
            state,
            count: 0,
            reason: Some(reason.into()),
        }
    }
}

// --- filtering + paging ----------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TimelineParams {
    /// CSV over [`KINDS`]. An unknown name is a 400 naming the vocabulary.
    #[serde(default)]
    pub kind: Option<String>,
    /// `human` | `agent` | `system`, or a literal author NAME.
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub since: Option<i64>,
    #[serde(default)]
    pub until: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
    /// Include the LIVE GitHub lane. Defaults to on for a PR-bound review.
    #[serde(default)]
    pub github: Option<bool>,
    /// A `kbc-hunkid/1` address — turns on the `turns` lane (loopback only).
    #[serde(default)]
    pub hunk: Option<String>,
    #[serde(default)]
    pub ps: Option<String>,
}

/// The filter, resolved once, so the count and the page share a predicate.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResolvedFilters {
    pub kinds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
}

impl ResolvedFilters {
    pub fn keeps(&self, e: &TimelineEvent) -> bool {
        if !self.kinds.is_empty() && !self.kinds.iter().any(|k| k == e.kind) {
            return false;
        }
        if let Some(a) = &self.author {
            let matches_kind = e.author.kind == a;
            let matches_name = e
                .author
                .name
                .as_deref()
                .map(|n| n.eq_ignore_ascii_case(a))
                .unwrap_or(false);
            if !matches_kind && !matches_name {
                return false;
            }
        }
        if let Some(s) = self.since {
            if e.ts < s {
                return false;
            }
        }
        if let Some(u) = self.until {
            if e.ts > u {
                return false;
            }
        }
        true
    }
}

pub fn resolve_filters(params: &TimelineParams) -> Result<ResolvedFilters, ApiError> {
    let mut kinds: Vec<String> = Vec::new();
    if let Some(raw) = &params.kind {
        for k in raw.split(',').map(str::trim).filter(|k| !k.is_empty()) {
            if !KINDS.contains(&k) {
                return Err(ApiError::bad_request(format!(
                    "unknown timeline kind {k:?} — the vocabulary is {KINDS:?}"
                )));
            }
            if !kinds.iter().any(|e| e == k) {
                kinds.push(k.to_string());
            }
        }
    }
    if let (Some(s), Some(u)) = (params.since, params.until) {
        if s > u {
            return Err(ApiError::bad_request(format!(
                "since ({s}) is after until ({u})"
            )));
        }
    }
    Ok(ResolvedFilters {
        kinds,
        author: params
            .author
            .as_ref()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty()),
        since: params.since,
        until: params.until,
    })
}

// --- route -----------------------------------------------------------------

/// `GET /api/reviews/{id}/timeline` — bearer.
///
/// `include_superseded`/`include_resolved` equivalents are NOT params: a
/// timeline is a HISTORY, so every row this review has ever touched is
/// always included (a superseded finding's own import/disposition events
/// still happened; hiding them would make the timeline lie about the past).
/// `?kind=`/`?author=`/`?since=`/`?until=` narrow the VIEW and always
/// report the true `total` they narrowed from.
pub async fn review_timeline_route(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<i64>,
    Query(params): Query<TimelineParams>,
    req: axum::http::Request<axum::body::Body>,
) -> Result<impl IntoResponse, ApiError> {
    let filters = resolve_filters(&params)?;
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = params.offset.unwrap_or(0);

    let (review, repo, repo_id) = require_review(&state, id).await?;
    let repo_root = repo.path.clone();

    // 2026-08-31 incident (store.rs module doc): every store read this
    // composition needs, in ONE blocking-pool trip.
    let ps_param = params.ps.clone();
    let (
        binding,
        patchsets,
        findings,
        annotations,
        verdict_published,
        docs,
        report,
        claim_rows,
        ps,
    ) = state
        .store
        .run_blocking(move |store| -> Result<_, ApiError> {
            let binding = store.get_review_pr_binding(id)?.unwrap_or_default();
            let patchsets = store.list_patchsets(id)?;
            let findings = store.list_review_findings(id, None, true)?;
            let annotations = store.list_review_annotations(id, true)?;
            let verdict_published = store.get_review_verdict_published(id)?;
            let docs = store.list_review_docs(id)?;
            let report = store.get_review_report(id)?.unwrap_or_default();
            let claim_rows = store.list_claims(
                &store::ClaimFilter {
                    repo_id,
                    review_id: Some(id),
                    ..Default::default()
                },
                MAX_CLAIMS,
                0,
            )?;
            let ps = crate::reviews::resolve_ps(store, id, ps_param.as_deref()).ok();
            Ok((
                binding,
                patchsets,
                findings,
                annotations,
                verdict_published,
                docs,
                report,
                claim_rows,
                ps,
            ))
        })
        .await?;

    let mut sources: Vec<LaneStatus> = Vec::new();
    let mut events = core_events(
        &review,
        &binding,
        &patchsets,
        &findings,
        &annotations,
        verdict_published.clone(),
    );
    sources.push(LaneStatus::ok(
        "lifecycle",
        events.iter().filter(|e| e.lane == "lifecycle").count(),
    ));
    sources.push(LaneStatus::ok(
        "findings",
        events.iter().filter(|e| e.lane == "findings").count(),
    ));
    sources.push(LaneStatus::ok(
        "verdict",
        events.iter().filter(|e| e.lane == "verdict").count(),
    ));
    sources.push(LaneStatus::ok(
        "comments",
        events.iter().filter(|e| e.lane == "comments").count(),
    ));

    // --- pr_body -----------------------------------------------------------
    let ps_number = ps.as_ref().map(|p| p.ps_number).unwrap_or(0);
    let pr_body = review_pseudo::render_pr_body(&binding);
    let latest_tip = patchsets.last().map(|p| p.tip_sha.clone());
    match pr_body_event(&binding, latest_tip.as_deref(), &pr_body) {
        Some(e) => {
            events.push(e);
            sources.push(LaneStatus::ok("pr_body", 1));
        }
        None => sources.push(LaneStatus::not_ok(
            "pr_body",
            LANE_SKIPPED,
            pr_body
                .reason
                .clone()
                .unwrap_or_else(|| "this review has no PR description snapshot".into()),
        )),
    }

    // --- document + report -------------------------------------------------
    let doc_events = doc_revision_events(&docs);
    sources.push(LaneStatus::ok("document", doc_events.len()));
    events.extend(doc_events);

    match report_event(&report) {
        Some(e) => {
            events.push(e);
            sources.push(LaneStatus::ok("report", 1));
        }
        None => sources.push(LaneStatus::not_ok(
            "report",
            LANE_SKIPPED,
            "this review has no agent report yet (`kb-code review report <id> --set`)",
        )),
    }

    // --- working-tree comments on this review's own files -------------------
    let (wt_events, wt_lane) = match &ps {
        Some(ps) => {
            let root = repo_root.clone();
            let base = ps.base_sha.clone();
            let tip = ps.tip_sha.clone();
            let store = state.store.clone();
            let rows = tokio::task::spawn_blocking(move || {
                let files = crate::reviews::files_changed(&root, &base, &tip).unwrap_or_default();
                let mut out: Vec<store::AnnotationRow> = Vec::new();
                for f in files {
                    if let Ok(rows) = store.list_annotations(repo_id, &f.path) {
                        out.extend(rows);
                    }
                }
                out
            })
            .await
            .map_err(|e| {
                ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;
            let evs = wt_comment_events(&rows);
            let n = evs.len();
            (evs, LaneStatus::ok("wt_comments", n))
        }
        None => (
            Vec::new(),
            LaneStatus::not_ok(
                "wt_comments",
                LANE_SKIPPED,
                "this review has no patchset, so it has no file set to scope working-tree \
                 comments to",
            ),
        ),
    };
    sources.push(wt_lane);
    events.extend(wt_events);

    // --- claims ------------------------------------------------------------
    let paths: BTreeSet<String> = claim_rows
        .iter()
        .filter_map(|c| c.subject_path.clone())
        .collect();
    let blobs = state
        .store
        .run_blocking(move |store| -> Result<BTreeMap<String, String>, ApiError> {
            let mut out = BTreeMap::new();
            for p in paths {
                if let Some(f) = store.get_file(repo_id, &p)? {
                    out.insert(p, f.blob_hash);
                }
            }
            Ok(out)
        })
        .await?;
    let claim_evs = claim_events(&claim_rows, &blobs);
    sources.push(LaneStatus::ok("claims", claim_evs.len()));
    events.extend(claim_evs);

    // --- github (live) ------------------------------------------------------
    let want_github = params.github.unwrap_or(binding.pr_number.is_some());
    if !want_github {
        sources.push(LaneStatus::not_ok(
            "github",
            LANE_SKIPPED,
            "the GitHub lane is a LIVE network call and was not requested (`?github=1`)",
        ));
    } else if let Some(pr_number) = binding.pr_number {
        let root_for_origin = repo_root.clone();
        let gh = tokio::task::spawn_blocking(move || crate::github::github_repo(&root_for_origin))
            .await
            .map_err(|e| {
                ApiError::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?;
        match gh {
            Ok(gh_repo) => {
                match state
                    .github
                    .list_pull_comments(&gh_repo.owner, &gh_repo.name, pr_number as u64)
                    .await
                {
                    Ok((list, truncated)) => {
                        let evs = github_comment_events(&list);
                        let n = evs.len();
                        events.extend(evs);
                        if truncated {
                            sources.push(LaneStatus {
                                lane: "github",
                                state: LANE_DEGRADED,
                                count: n,
                                reason: Some(
                                    "GitHub returned more comments than this daemon will page \
                                     through; the newest are here and the rest are on \
                                     `/github-threads`"
                                        .into(),
                                ),
                            });
                        } else {
                            sources.push(LaneStatus::ok("github", n));
                        }
                    }
                    Err(e) => sources.push(LaneStatus::not_ok(
                        "github",
                        LANE_DEGRADED,
                        format!("GitHub could not be read: {e}"),
                    )),
                }
            }
            Err(e) => sources.push(LaneStatus::not_ok(
                "github",
                LANE_DEGRADED,
                format!("this repo has no GitHub origin: {e}"),
            )),
        }
    } else {
        sources.push(LaneStatus::not_ok(
            "github",
            LANE_SKIPPED,
            "this review is not bound to a pull request",
        ));
    }

    // --- turns (loopback + ?hunk= only) -------------------------------------
    match (&params.hunk, &ps) {
        (None, _) => sources.push(LaneStatus::not_ok(
            "turns",
            LANE_SKIPPED,
            "the hunk↔turn lane answers about ONE hunk — pass `?hunk=<kbc-hunkid/1>`",
        )),
        (Some(_), None) => sources.push(LaneStatus::not_ok(
            "turns",
            LANE_SKIPPED,
            "this review has no patchset to locate a hunk in",
        )),
        (Some(hunk), Some(_)) => {
            if !kb_server::middleware::request_is_loopback(&req, &state.auth.trusted_proxies) {
                sources.push(LaneStatus::not_ok(
                    "turns",
                    LANE_REFUSED,
                    "the hunk↔turn join reads raw transcript content, which never leaves \
                     loopback (the `raw-transcript` sensitivity class)",
                ));
            } else {
                match crate::review_turns::turns_for_hunk(&state, id, hunk, params.ps.as_deref())
                    .await
                {
                    Ok(out) => {
                        let evs = turn_events(&out.turns);
                        let n = evs.len();
                        events.extend(evs);
                        if let Some(reason) = out.reason {
                            sources.push(LaneStatus {
                                lane: "turns",
                                state: LANE_OK,
                                count: n,
                                reason: Some(reason),
                            });
                        } else {
                            sources.push(LaneStatus::ok("turns", n));
                        }
                    }
                    Err(e) => sources.push(LaneStatus::not_ok(
                        "turns",
                        LANE_DEGRADED,
                        format!("the hunk↔turn join could not run: {}", e.message_text()),
                    )),
                }
            }
        }
    }

    // --- order, filter, page ------------------------------------------------
    events.sort_by_key(|e| e.ts);
    let kept: Vec<&TimelineEvent> = events.iter().filter(|e| filters.keeps(e)).collect();
    let total = kept.len();
    let mut page_events: Vec<TimelineEvent> =
        kept.into_iter().skip(offset).take(limit).cloned().collect();

    // V76-B3 (kbc-prose/1) — `body_md` refs for the PAGE's events only (a
    // filtered-out event never pays for resolution), one blocking-pool trip,
    // computed per request and persisted nowhere.
    {
        let bodies: Vec<Option<String>> = page_events.iter().map(|e| e.body_md.clone()).collect();
        let refs = state
            .store
            .run_blocking(
                move |store| -> Result<Vec<Option<crate::prose_refs::FieldRefs>>, ApiError> {
                    let ctx = crate::prose_refs::RefCtx {
                        repo_id,
                        review_id: Some(id),
                        ps_number: ps.as_ref().map(|p| p.ps_number),
                    };
                    bodies
                        .iter()
                        .map(|b| match b {
                            Some(b) => crate::prose_refs::field_refs(store, &ctx, b).map(Some),
                            None => Ok(None),
                        })
                        .collect()
                },
            )
            .await?;
        for (e, r) in page_events.iter_mut().zip(refs) {
            e.body_refs = r;
        }
    }
    let page: Vec<serde_json::Value> = page_events.iter().map(TimelineEvent::to_value).collect();

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({
            "schema": SCHEMA,
            "review_id": id,
            "repo": review.repo,
            "ps_number": ps_number,
            "total": total,
            "returned": page.len(),
            "offset": offset,
            "limit": limit,
            "filters": filters,
            "sources": sources,
            "events": page,
        })),
    ))
}

// --- route contract (invariant 15) ----------------------------------------

use crate::entities::RouteContract;

fn timeline_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("kind", "comment")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<TimelineParams>(serde_json::Value::Object(map)).is_ok()
}

pub const TIMELINE_ROUTE: RouteContract = RouteContract {
    path: "/api/reviews/{id}/timeline",
    handler: "review_timeline::review_timeline_route",
    required_params: &[],
    params_accept_without: timeline_accept_without,
};

/// V73-K3's route registry (invariant 15). Collected HERE because the
/// timeline is this unit's headline surface and the other three exist to
/// feed it; the per-module consts stay next to their handlers so a route
/// and its contract are edited together.
pub const V73_K3_ROUTES: &[RouteContract] = &[
    TIMELINE_ROUTE,
    crate::claims::CLAIMS_ROUTE,
    crate::review_pseudo::PSEUDO_LIST_ROUTE,
    crate::review_pseudo::PSEUDO_FILE_ROUTE,
    crate::review_turns::HUNK_TURNS_ROUTE,
];

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn base_review() -> store::ReviewRow {
        store::ReviewRow {
            id: 1,
            repo: "r".into(),
            title: Some("t".into()),
            base_ref: "main".into(),
            head_ref: "feature".into(),
            session_id: None,
            state: "open".into(),
            created_at: 100,
            updated_at: 100,
            verdict: None,
            verdict_note: None,
            verdict_at: None,
            verdict_ps: None,
        }
    }

    pub(super) fn ps(ps_number: i64, captured_at: i64) -> store::ReviewPatchsetRow {
        store::ReviewPatchsetRow {
            id: ps_number,
            review_id: 1,
            ps_number,
            tip_sha: format!("sha{ps_number}"),
            base_sha: "base".into(),
            captured_at,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finding(
        slug: &str,
        origin: &str,
        import_batch_id: &str,
        created_at: i64,
        disposition: Option<(&str, i64)>,
        published_at: Option<i64>,
    ) -> store::ReviewFindingRow {
        store::ReviewFindingRow {
            id: 0,
            review_id: 1,
            annotation_id: format!("ann-{slug}"),
            slug: slug.to_string(),
            severity: "concern".into(),
            category: "cat".into(),
            location_kind: "whole_file".into(),
            location_path: "a.rb".into(),
            location_lines: None,
            location_removed: false,
            title: format!("title-{slug}"),
            rationale: "r".into(),
            recommendation: None,
            evidence_lang: None,
            evidence_source: None,
            origin: origin.to_string(),
            author: Some("claude".into()),
            disposition: disposition.map(|(s, _)| s.to_string()),
            disposition_note: None,
            disposition_by: disposition.map(|_| "you".to_string()),
            disposition_at: disposition.map(|(_, at)| at),
            content_updated_at: None,
            published_state: if published_at.is_some() {
                "published".into()
            } else {
                "unpublished".into()
            },
            published_at,
            published_url: published_at.map(|_| "https://github.com/x/y/pull/1".to_string()),
            superseded: false,
            superseded_at: None,
            superseded_reason: None,
            import_batch_id: import_batch_id.to_string(),
            created_at,
            updated_at: created_at,
            act: "issue".into(),
            blocking: false,
            cites_json: None,
            fingerprint: None,
            superseded_by: None,
        }
    }

    pub(super) fn note(
        id: &str,
        parent_id: Option<&str>,
        intent: &str,
        created_at: i64,
    ) -> store::AnnotationRow {
        store::AnnotationRow {
            id: id.to_string(),
            repo_id: 1,
            path: "a.rb".into(),
            anchor: parent_id.is_none().then(|| "{}".to_string()),
            anchor_kind: "whole_file".into(),
            anchor2: None,
            parent_id: parent_id.map(|s| s.to_string()),
            intent: intent.to_string(),
            body: "hi".into(),
            author: "you".into(),
            created_at,
            updated_at: created_at,
            resolved: false,
            review_id: Some(1),
            ps_number: Some(1),
            side: None,
            set_id: None,
            trail_id: None,
        }
    }

    #[test]
    fn empty_review_has_only_the_review_created_event() {
        let review = base_review();
        let binding = store::ReviewPrBinding::default();
        let events = compose_review_timeline(&review, &binding, &[], &[], &[], None);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "review_created");
        assert_eq!(events[0]["at"], 100);
    }

    /// Fixed-clock fixture rows → an EXACT event sequence (VERIFY plan's
    /// own ask), covering every kind in one composition.
    #[test]
    fn composes_every_kind_in_ascending_at_order() {
        let mut review = base_review();
        review.verdict = Some("approve".into());
        review.verdict_at = Some(500);

        let binding = store::ReviewPrBinding {
            pr_number: Some(42),
            pr_repo_slug: Some("acme/widget".into()),
            pr_meta_fetched_at: Some(150),
            ..store::ReviewPrBinding::default()
        };

        let patchsets = vec![ps(1, 110), ps(2, 300)];

        let findings = vec![
            // A real import batch: two findings, same batch id + same
            // created_at (as `reconcile_findings_import` guarantees).
            finding("f-a", "import", "batch_1", 200, None, None),
            finding("f-b", "import", "batch_1", 200, Some(("agree", 250)), None),
            // A manual finding — its OWN event, not folded into a "manual"
            // batch group.
            finding("f-c", "manual", "manual", 220, None, Some(400)),
        ];

        let annotations = vec![
            // A finding's own top-level annotation row — must NOT surface
            // as a separate "comment" event (it's already `finding_added`/
            // `findings_import`).
            note("ann-f-a", None, "finding", 200),
            // A reply on that finding's thread — DOES count as "comment".
            note("reply-1", Some("ann-f-a"), "question", 260),
            // An ordinary review-level note.
            note("note-1", None, "note", 130),
        ];

        let events = compose_review_timeline(
            &review,
            &binding,
            &patchsets,
            &findings,
            &annotations,
            Some((
                Some(450),
                Some("https://github.com/x/y/pull/1#review-1".to_string()),
            )),
        );

        let kinds_and_at: Vec<(String, i64)> = events
            .iter()
            .map(|e| {
                (
                    e["kind"].as_str().unwrap().to_string(),
                    e["at"].as_i64().unwrap(),
                )
            })
            .collect();

        assert_eq!(
            kinds_and_at,
            vec![
                ("review_created".to_string(), 100),
                ("patchset".to_string(), 110),
                ("comment".to_string(), 130),
                ("pr_bound".to_string(), 150),
                ("findings_import".to_string(), 200),
                ("finding_added".to_string(), 220),
                ("disposition".to_string(), 250),
                ("comment".to_string(), 260),
                ("patchset".to_string(), 300),
                ("finding_published".to_string(), 400),
                ("verdict_published".to_string(), 450),
                ("verdict".to_string(), 500),
            ]
        );

        // Spot-check a few payloads.
        let import_evt = events
            .iter()
            .find(|e| e["kind"] == "findings_import")
            .unwrap();
        assert_eq!(import_evt["import_batch_id"], "batch_1");
        assert_eq!(import_evt["count"], 2);
        assert_eq!(import_evt["slugs"], serde_json::json!(["f-a", "f-b"]));

        let manual_evt = events
            .iter()
            .find(|e| e["kind"] == "finding_added")
            .unwrap();
        assert_eq!(manual_evt["slug"], "f-c");

        let comment_evts: Vec<&serde_json::Value> =
            events.iter().filter(|e| e["kind"] == "comment").collect();
        assert_eq!(
            comment_evts.len(),
            2,
            "the finding's own top-level row must be excluded"
        );
        assert!(comment_evts.iter().any(|e| e["annotation_id"] == "note-1"));
        assert!(comment_evts
            .iter()
            .any(|e| e["annotation_id"] == "reply-1" && e["is_reply"] == true));
        // V73-K2c regression test: a `comment` event's v2 envelope `author`
        // must survive as the STRUCTURED object, not be clobbered by a
        // duplicate flat "author" string in `detail` (both `note()` fixture
        // rows are authored "you", a human). Before the fix this failed —
        // `e["author"]` was the bare string `"you"`, so `e["author"]["kind"]`
        // indexed into a JSON string and read back `Value::Null`.
        assert!(comment_evts
            .iter()
            .all(|e| e["author"]["kind"] == "human" && e["author"]["name"] == "you"));
    }

    #[test]
    fn manual_findings_never_collapse_into_one_shared_batch_event() {
        let review = base_review();
        let binding = store::ReviewPrBinding::default();
        let findings = vec![
            finding("f-a", "manual", "manual", 100, None, None),
            finding("f-b", "manual", "manual", 999, None, None),
        ];
        let events = compose_review_timeline(&review, &binding, &[], &findings, &[], None);
        let finding_added: Vec<&serde_json::Value> = events
            .iter()
            .filter(|e| e["kind"] == "finding_added")
            .collect();
        assert_eq!(
            finding_added.len(),
            2,
            "each manual finding gets its OWN event"
        );
        assert!(finding_added
            .iter()
            .any(|e| e["at"] == 100 && e["slug"] == "f-a"));
        assert!(finding_added
            .iter()
            .any(|e| e["at"] == 999 && e["slug"] == "f-b"));
        assert!(
            events.iter().all(|e| e["kind"] != "findings_import"),
            "no manual finding should ever produce a findings_import event"
        );
    }

    #[test]
    fn recomposing_the_same_state_is_byte_identical() {
        let review = base_review();
        let binding = store::ReviewPrBinding::default();
        let patchsets = vec![ps(1, 110), ps(2, 110)]; // tie on `at`
        let a = compose_review_timeline(&review, &binding, &patchsets, &[], &[], None);
        let b = compose_review_timeline(&review, &binding, &patchsets, &[], &[], None);
        assert_eq!(a, b);
    }
}

#[cfg(test)]
mod v2_tests {
    use super::*;
    use crate::review_timeline::tests as v1;

    #[test]
    fn the_agent_author_set_tracks_kbs_harness_vocabulary() {
        for h in kb_core::sessions::HARNESSES {
            assert!(
                AGENT_AUTHOR_NAMES.contains(&h),
                "kb added the harness {h:?}; kb-code's timeline would read it as a HUMAN author"
            );
        }
        assert_eq!(
            AGENT_AUTHOR_NAMES.len(),
            kb_core::sessions::HARNESSES.len() + 1
        );
    }

    #[test]
    fn an_author_kind_is_derived_from_the_name_and_nothing_else() {
        assert_eq!(author_for(None).kind, AUTHOR_SYSTEM);
        assert_eq!(author_for(Some("claude")).kind, AUTHOR_AGENT);
        assert_eq!(author_for(Some("CLAUDE")).kind, AUTHOR_AGENT);
        assert_eq!(author_for(Some("you")).kind, AUTHOR_HUMAN);
        assert_eq!(author_for(Some("nik")).kind, AUTHOR_HUMAN);
        assert_eq!(author_for(Some("you")).name.as_deref(), Some("you"));
    }

    /// The additive contract, pinned: every event a v1 reader saw still
    /// carries `at` and its own payload keys, unchanged.
    #[test]
    fn v1_payload_keys_are_byte_identical_under_v2() {
        let review = v1::base_review();
        let findings = vec![v1::finding(
            "f-a",
            store::FINDING_ORIGIN_IMPORT,
            "batch_1",
            300,
            Some(("agree", 400)),
            Some(500),
        )];
        let events = compose_review_timeline(
            &review,
            &store::ReviewPrBinding::default(),
            &[v1::ps(1, 200)],
            &findings,
            &[],
            None,
        );
        let import = events
            .iter()
            .find(|e| e["kind"] == "findings_import")
            .expect("import event");
        assert_eq!(import["at"], 300);
        assert_eq!(import["import_batch_id"], "batch_1");
        assert_eq!(import["count"], 1);
        assert_eq!(import["slugs"], serde_json::json!(["f-a"]));

        let disp = events
            .iter()
            .find(|e| e["kind"] == "disposition")
            .expect("disposition");
        assert_eq!(disp["slug"], "f-a");
        assert_eq!(disp["state"], "agree");
        assert_eq!(disp["by"], "you");

        let created = events
            .iter()
            .find(|e| e["kind"] == "review_created")
            .expect("created");
        assert_eq!(created["review_id"], 1);
        assert_eq!(created["repo"], "r");

        // …and every one gained the v2 envelope.
        for e in &events {
            assert_eq!(e["ts"], e["at"], "ts mirrors at on {}", e["kind"]);
            assert!(e["author"]["kind"].is_string(), "{}", e["kind"]);
            assert!(LANES.contains(&e["lane"].as_str().unwrap_or("")));
            assert!(KINDS.contains(&e["kind"].as_str().unwrap_or("")));
        }
    }

    #[test]
    fn every_kind_belongs_to_a_declared_lane_and_the_vocabularies_are_closed() {
        assert_eq!(LANES.len(), 11);
        assert_eq!(KINDS.len(), 17);
        let mut sorted = KINDS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), KINDS.len(), "KINDS has a duplicate");
    }

    fn ev(at: i64, kind: &'static str, author: EventAuthor) -> TimelineEvent {
        TimelineEvent::new(at, kind, "comments", author)
    }

    #[test]
    fn filters_narrow_the_view_and_never_reorder_it() {
        let events = [
            ev(10, "comment", author_for(Some("you"))),
            ev(20, "comment", author_for(Some("claude"))),
            ev(30, "verdict", EventAuthor::system()),
        ];
        let by_kind = ResolvedFilters {
            kinds: vec!["comment".into()],
            ..Default::default()
        };
        assert_eq!(events.iter().filter(|e| by_kind.keeps(e)).count(), 2);

        let by_author_kind = ResolvedFilters {
            author: Some("agent".into()),
            ..Default::default()
        };
        assert_eq!(events.iter().filter(|e| by_author_kind.keeps(e)).count(), 1);

        let by_author_name = ResolvedFilters {
            author: Some("YOU".into()),
            ..Default::default()
        };
        assert_eq!(events.iter().filter(|e| by_author_name.keeps(e)).count(), 1);

        let windowed = ResolvedFilters {
            since: Some(20),
            until: Some(25),
            ..Default::default()
        };
        assert_eq!(events.iter().filter(|e| windowed.keeps(e)).count(), 1);

        let none = ResolvedFilters::default();
        assert_eq!(events.iter().filter(|e| none.keeps(e)).count(), 3);
    }

    #[test]
    fn an_unknown_kind_is_a_400_naming_the_vocabulary() {
        let err = resolve_filters(&TimelineParams {
            kind: Some("comment,not_a_kind".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(err.message_text().contains("not_a_kind"));
        assert!(err.message_text().contains("review_created"));
    }

    #[test]
    fn an_inverted_window_is_refused_rather_than_silently_empty() {
        assert!(resolve_filters(&TimelineParams {
            since: Some(100),
            until: Some(10),
            ..Default::default()
        })
        .is_err());
    }

    #[test]
    fn the_pr_body_event_reports_head_drift_and_never_invents_a_revision() {
        let binding = store::ReviewPrBinding {
            pr_number: Some(7),
            pr_repo_slug: Some("acme/acme-app".into()),
            pr_head_sha: Some("aaaaaa".into()),
            pr_meta_json: Some(
                serde_json::json!({"body": "why this change", "author": "you"}).to_string(),
            ),
            pr_meta_fetched_at: Some(900),
            artifact_hint_kb: None,
            artifact_hint_id: None,
        };
        let file = review_pseudo::render_pr_body(&binding);
        let e = pr_body_event(&binding, Some("bbbbbb"), &file).expect("event");
        assert_eq!(e.kind, "pr_body");
        assert_eq!(e.ts, 900);
        assert_eq!(e.body_md.as_deref(), Some("why this change"));
        assert_eq!(e.author.kind, AUTHOR_HUMAN);
        let drift = e.drift.expect("drift");
        assert_eq!(drift.kind, "pr_head");
        assert!(drift.note.contains("aaaaaa") && drift.note.contains("bbbbbb"));

        // Same head as the newest patchset ⇒ no drift claim at all.
        let e = pr_body_event(&binding, Some("aaaaaa"), &file).unwrap();
        assert!(e.drift.is_none());
    }

    #[test]
    fn an_unbound_review_has_no_pr_body_event() {
        let binding = store::ReviewPrBinding::default();
        let file = review_pseudo::render_pr_body(&binding);
        assert!(pr_body_event(&binding, None, &file).is_none());
    }

    #[test]
    fn a_drifted_claim_greys_with_a_caption_and_a_pinned_one_does_not() {
        let row = store::ClaimRow {
            id: "clm_a".into(),
            repo_id: 1,
            subject_kind: "path".into(),
            subject: "app/models/order.rb".into(),
            subject_path: Some("app/models/order.rb".into()),
            review_id: Some(1),
            kind: "explain".into(),
            body_md: "the retry path".into(),
            confidence: Some(0.8),
            evidence_json: "[]".into(),
            session_id: Some("sess-1".into()),
            model: Some("claude".into()),
            blob_sha: Some("aaa".into()),
            created_at: 42,
        };
        let mut blobs = BTreeMap::new();
        blobs.insert("app/models/order.rb".to_string(), "bbb".to_string());
        let e = &claim_events(std::slice::from_ref(&row), &blobs)[0];
        assert_eq!(e.kind, "claim");
        assert_eq!(e.author.kind, AUTHOR_AGENT);
        assert_eq!(e.author.session_id.as_deref(), Some("sess-1"));
        assert_eq!(e.detail["state"], claims::STATE_DRIFTED);
        assert!(e.drift.as_ref().unwrap().note.contains("aaa"));
        assert_eq!(e.r#ref.as_deref(), Some("code:app/models/order.rb"));

        blobs.insert("app/models/order.rb".to_string(), "aaa".to_string());
        let e = &claim_events(std::slice::from_ref(&row), &blobs)[0];
        assert_eq!(e.detail["state"], claims::STATE_PINNED);
        assert!(e.drift.is_none());
    }

    #[test]
    fn a_document_revision_carries_the_authors_model_and_session() {
        let row = store::ReviewDocRow {
            id: 1,
            review_id: 1,
            ps_number: 1,
            revision: 2,
            schema: "kbc-review/1".into(),
            tier: "standard".into(),
            doc_md: "---\n---\nbody".into(),
            summary_md: "the summary".into(),
            risk_level: Some("low".into()),
            risk_why: None,
            omitted_json: "[\"flows\"]".into(),
            author_json: Some(
                serde_json::json!({"kind": "agent", "model": "opus-5", "session_id": "s-9"})
                    .to_string(),
            ),
            byte_len: 12,
            created_at: 700,
        };
        let e = &doc_revision_events(std::slice::from_ref(&row))[0];
        assert_eq!(e.kind, "doc_revision");
        assert_eq!(e.detail["revision"], 2);
        assert_eq!(e.detail["omitted"], serde_json::json!(["flows"]));
        assert_eq!(e.author.kind, AUTHOR_AGENT);
        assert_eq!(e.author.model.as_deref(), Some("opus-5"));
        assert_eq!(e.author.session_id.as_deref(), Some("s-9"));
        assert_eq!(e.body_md.as_deref(), Some("the summary"));
        assert_eq!(e.r#ref.as_deref(), Some("code:~review/review.md"));
    }

    #[test]
    fn a_working_tree_comment_is_its_own_kind_never_a_review_comment() {
        let rows = vec![
            store::AnnotationRow {
                review_id: None,
                path: "app/models/order.rb".into(),
                ..v1::note("wt-1", None, "note", 10)
            },
            v1::note("rev-1", None, "note", 20),
        ];
        let evs = wt_comment_events(&rows);
        assert_eq!(
            evs.len(),
            1,
            "a review-scoped row is NOT a working-tree comment"
        );
        assert_eq!(evs[0].kind, "wt_comment");
        assert_eq!(evs[0].detail["annotation_id"], "wt-1");
        // V73-K2c regression test: same collision as the `comment` lane's
        // own (see `composes_every_kind_in_ascending_at_order`'s tail) —
        // must be checked through `to_value()`, since the in-memory
        // `.detail` map above never collides with the struct's own
        // `author` field (only the FLATTENED serialization does).
        let v = evs[0].to_value();
        assert_eq!(v["author"]["kind"], "human");
        assert_eq!(v["author"]["name"], "you");
    }

    #[test]
    fn a_turn_event_carries_the_bearer_visible_half_and_no_transcript_prose() {
        let m = crate::review_turns::TurnMatch {
            turn_id: "t-0189aa112233".into(),
            session_id: "sess-1".into(),
            uuid: "0189aa11-2233-4455-6677-8899aabbccdd".into(),
            ts: 1_700_000_000_000,
            tool: "Edit".into(),
            path: "app/models/order.rb".into(),
            tier: crate::review_turns::TIER_EXACT,
            why: "three witnesses".into(),
            commit: Some("abc123".into()),
            join_via: Some("trailer".into()),
            matched_bytes: 40,
            kb_read: "kb sessions read sess-1 --turn t-0189aa112233".into(),
        };
        let e = &turn_events(std::slice::from_ref(&m))[0];
        assert_eq!(e.kind, "turn");
        assert_eq!(
            e.ts, 1_700_000_000,
            "milliseconds are normalised to seconds"
        );
        assert_eq!(e.detail["turn_id"], "t-0189aa112233");
        assert_eq!(e.detail["tier"], "exact");
        assert!(
            e.body_md.is_none(),
            "a turn event never carries transcript prose"
        );
    }

    #[test]
    fn every_lane_status_state_is_from_the_closed_set() {
        for s in [LANE_OK, LANE_SKIPPED, LANE_REFUSED, LANE_DEGRADED] {
            assert!(!s.is_empty());
        }
        let l = LaneStatus::not_ok("github", LANE_DEGRADED, "GitHub had a bad day");
        assert_eq!(l.count, 0);
        assert!(l.reason.is_some(), "a non-ok lane ALWAYS says why");
    }

    #[test]
    fn the_route_contracts_all_accept_a_full_query() {
        for c in V73_K3_ROUTES {
            assert!((c.params_accept_without)(""), "{}", c.path);
            for p in c.required_params {
                assert!(!(c.params_accept_without)(p), "{} / {p}", c.path);
            }
        }
    }
}
