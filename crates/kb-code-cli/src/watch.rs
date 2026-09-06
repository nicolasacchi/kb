//! `kb-code annotate watch` — SSE-driven triage loop for annotation /
//! suggestion / verdict / finding activity.
//!
//! Modeled on kb-cli's `comments_watch.rs`: an initial fetch seeds a
//! seen-set (no surface unless `--backlog`); SSE events trigger a
//! refetch; only NEW/CHANGED items are printed. The daemon's
//! `/api/events` replays the bus ring from cursor 0 on a cold connect,
//! so the seen-set is what keeps replay from double-surfacing.
//!
//! The loop shell is thin. Seen-set / scope / emit formatting live in
//! pure functions so they can be unit-tested without a daemon.
//!
//! PRR-R6 (design doc §3.3, kb v0.39 T2 Phase 6) added the `Finding`
//! variant: question threads on a finding are just replies on its own
//! `annotations` row, so they already surface through
//! `Annotation(AnnotationKey)` today (an edit to `updated_at`/`resolved`
//! is exactly what that key detects, and `items_from_comments` already
//! walks every finding's linked annotation) — no new machinery needed
//! for "questions." The only genuinely new surfaceable change is a
//! finding's OWN metadata (severity/disposition/supersession/publish
//! state), polled from `GET /api/reviews/{id}/findings` alongside the
//! existing `/comments` poll whenever the scope is review-bound.

use crate::sse::{self, Frame};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;
use tokio::time::Instant;

/// Backoff schedule on disconnect — mirrors `kb comments watch` / `kb push`.
const BACKOFF_STEPS: &[u64] = &[1, 2, 4, 8, 16, 30];
/// A connection up at least this long resets the backoff on a clean close.
const STABLE_SESSION: Duration = Duration::from_secs(5);

/// Dedupe key for a surfaced item. Annotation rows key on the four-tuple
/// the brief names; verdicts are a separate namespace so a verdict
/// flip never collides with an annotation id; findings are a THIRD
/// separate namespace (PRR-R6, design doc §3.3) keyed on the finding's
/// own metadata (never the linked annotation's), so a finding's
/// disposition/supersession/publish-state changes surface independently
/// of any question-thread activity on the same annotation row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SeenKey {
    Annotation(AnnotationKey),
    Verdict(VerdictKey),
    Finding(FindingKey),
}

/// `(id, updated_at, resolved, suggestion_applied)` — a change to any
/// field is a new surfaceable item.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AnnotationKey {
    pub id: String,
    pub updated_at: i64,
    pub resolved: bool,
    pub suggestion_applied: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VerdictKey {
    pub review_id: i64,
    pub state: String,
    pub stale: bool,
    pub at: i64,
}

/// `(review_id, slug, updated_at, disposition, superseded,
/// published_state)` (design doc §3.3, exact shape) — a change to any
/// field is a new surfaceable finding-metadata item. Deliberately
/// excludes everything that already rides `AnnotationKey` (body/replies/
/// resolved/suggestion) — that's the linked annotation's own job.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FindingKey {
    pub review_id: i64,
    pub slug: String,
    pub updated_at: i64,
    pub disposition: Option<String>,
    pub superseded: bool,
    pub published_state: String,
}

/// One thing the loop can print. Named (not a tuple) so clippy stays
/// happy and the emit path stays readable.
#[derive(Clone, Debug, PartialEq)]
pub enum WatchItem {
    Annotation(AnnotationItem),
    Verdict(VerdictItem),
    Finding(FindingItem),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationItem {
    pub id: String,
    pub path: String,
    pub line: Option<u32>,
    pub author: String,
    pub intent: String,
    pub body: String,
    pub resolved: bool,
    pub suggestion_applied: bool,
    pub suggestion_pending: bool,
    pub orphaned: bool,
    pub original_ps: Option<i64>,
    pub original_line: Option<u32>,
    pub reply_count: usize,
    pub review_id: Option<i64>,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VerdictItem {
    pub review_id: i64,
    pub state: String,
    pub stale: bool,
    pub at: i64,
    pub note: Option<String>,
}

/// A finding's own metadata, from one row of `GET
/// /api/reviews/{id}/findings` (`review-findings/1`, see
/// `review_findings.rs::finding_json` for the wire shape this parses).
#[derive(Clone, Debug, PartialEq)]
pub struct FindingItem {
    pub review_id: i64,
    pub slug: String,
    pub severity: String,
    pub category: String,
    pub location_path: String,
    pub location_line: Option<i64>,
    pub title: String,
    /// The finding row's own `author` field — the server already
    /// defaults this to `"you"` for a manual finding or `"claude"` for
    /// an import, so no client-side origin fallback is needed here.
    pub author: String,
    pub disposition: Option<String>,
    pub disposition_note: Option<String>,
    pub disposition_by: Option<String>,
    pub disposition_at: Option<i64>,
    pub superseded: bool,
    pub superseded_reason: Option<String>,
    pub published_state: String,
    pub orphaned: bool,
    pub confidence: String,
    pub thread_count: usize,
    pub unresolved_count: usize,
    pub updated_at: i64,
}

/// Repo and/or review scope. At least one side is set (enforced by the
/// clap / run entry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchScope {
    pub repo: Option<String>,
    pub review_id: Option<i64>,
}

impl WatchScope {
    pub fn new(repo: Option<String>, review_id: Option<i64>) -> Result<Self> {
        if repo.is_none() && review_id.is_none() {
            anyhow::bail!("annotate watch: pass --repo R and/or --review N");
        }
        Ok(Self { repo, review_id })
    }
}

impl WatchItem {
    pub fn seen_key(&self) -> SeenKey {
        match self {
            WatchItem::Annotation(a) => SeenKey::Annotation(AnnotationKey {
                id: a.id.clone(),
                updated_at: a.updated_at,
                resolved: a.resolved,
                suggestion_applied: a.suggestion_applied,
            }),
            WatchItem::Verdict(v) => SeenKey::Verdict(VerdictKey {
                review_id: v.review_id,
                state: v.state.clone(),
                stale: v.stale,
                at: v.at,
            }),
            WatchItem::Finding(f) => SeenKey::Finding(FindingKey {
                review_id: f.review_id,
                slug: f.slug.clone(),
                updated_at: f.updated_at,
                disposition: f.disposition.clone(),
                superseded: f.superseded,
                published_state: f.published_state.clone(),
            }),
        }
    }

    /// Design doc §3.3: `disposition_by` when a disposition was just
    /// set (the human-in-the-browser action an agent loop most wants to
    /// react to), else the finding row's own `author` field — which the
    /// server already resolves to the manual author or the import's
    /// `"claude"` fallback, so no second fallback is needed here. This
    /// is what makes `--ignore-author claude` suppress an agent's own
    /// import while still surfacing a human's disposition on that same
    /// finding.
    pub fn author(&self) -> Option<&str> {
        match self {
            WatchItem::Annotation(a) => Some(a.author.as_str()),
            WatchItem::Verdict(_) => None,
            WatchItem::Finding(f) => Some(f.disposition_by.as_deref().unwrap_or(f.author.as_str())),
        }
    }
}

/// Pull surfaceable annotation items from `GET /api/annotations/open`.
/// `suggestion_applied` is not on that wire shape — treated as false
/// (no repo-wide suggestion listing exists).
pub fn items_from_open(body: &Value) -> Vec<WatchItem> {
    let mut out = Vec::new();
    let Some(anns) = body.get("annotations").and_then(|v| v.as_array()) else {
        return out;
    };
    for a in anns {
        if let Some(item) = annotation_from_open_row(a) {
            out.push(WatchItem::Annotation(item));
        }
    }
    out
}

fn annotation_from_open_row(a: &Value) -> Option<AnnotationItem> {
    let id = a.get("id")?.as_str()?.to_string();
    Some(AnnotationItem {
        id,
        path: a
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        line: a.get("line").and_then(|v| v.as_u64()).map(|n| n as u32),
        author: a
            .get("author")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        intent: a
            .get("intent")
            .and_then(|v| v.as_str())
            .unwrap_or("note")
            .to_string(),
        body: a
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        resolved: a.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false),
        suggestion_applied: false,
        suggestion_pending: false,
        orphaned: false,
        original_ps: None,
        original_line: None,
        reply_count: a.get("reply_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        review_id: a.get("review_id").and_then(|v| v.as_i64()),
        updated_at: a.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

/// Pull surfaceable items from `GET /api/reviews/{id}/comments`
/// (`review-comments/1`). Walks every group, every parent comment.
pub fn items_from_comments(body: &Value) -> Vec<WatchItem> {
    let mut out = Vec::new();
    let review_id = body.get("review_id").and_then(|v| v.as_i64());
    let Some(groups) = body.get("groups").and_then(|v| v.as_array()) else {
        return out;
    };
    for g in groups {
        let path = g
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let Some(comments) = g.get("comments").and_then(|v| v.as_array()) else {
            continue;
        };
        for c in comments {
            if let Some(item) = annotation_from_comment(c, &path, review_id) {
                out.push(WatchItem::Annotation(item));
            }
        }
    }
    out
}

fn annotation_from_comment(
    c: &Value,
    path: &str,
    review_id: Option<i64>,
) -> Option<AnnotationItem> {
    let id = c.get("id")?.as_str()?.to_string();
    let resolution = c.get("resolution");
    let orphaned = resolution
        .and_then(|r| r.get("orphaned"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let line = resolution
        .and_then(|r| r.get("line"))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let original = resolution.and_then(|r| r.get("original"));
    let suggestion = c.get("suggestion");
    let has_suggestion = suggestion.map(|s| !s.is_null()).unwrap_or(false);
    let suggestion_applied = suggestion
        .and_then(|s| s.get("applied"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let replies = c
        .get("replies")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    Some(AnnotationItem {
        id,
        path: c
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(path)
            .to_string(),
        line,
        author: c
            .get("author")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        intent: c
            .get("intent")
            .and_then(|v| v.as_str())
            .unwrap_or("note")
            .to_string(),
        body: c
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        resolved: c.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false),
        suggestion_applied,
        suggestion_pending: has_suggestion && !suggestion_applied,
        orphaned,
        original_ps: original.and_then(|o| o.get("ps")).and_then(|v| v.as_i64()),
        original_line: original
            .and_then(|o| o.get("line"))
            .and_then(|v| v.as_u64())
            .map(|n| n as u32),
        reply_count: replies,
        review_id,
        updated_at: c.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

/// Pull surfaceable items from `GET /api/reviews/{id}/findings`
/// (`review-findings/1`, `include_superseded=true` so a supersession
/// SURFACES once rather than silently vanishing from the polled set —
/// design doc §3.3).
pub fn items_from_findings(body: &Value) -> Vec<WatchItem> {
    let mut out = Vec::new();
    let review_id = body.get("review_id").and_then(|v| v.as_i64());
    let Some(findings) = body.get("findings").and_then(|v| v.as_array()) else {
        return out;
    };
    for f in findings {
        if let Some(item) = finding_from_row(f, review_id) {
            out.push(WatchItem::Finding(item));
        }
    }
    out
}

fn finding_from_row(f: &Value, review_id: Option<i64>) -> Option<FindingItem> {
    let slug = f.get("slug")?.as_str()?.to_string();
    let review_id = f.get("review_id").and_then(|v| v.as_i64()).or(review_id)?;
    let disposition_obj = f.get("disposition").filter(|v| !v.is_null());
    let disposition = disposition_obj
        .and_then(|d| d.get("state"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let disposition_note = disposition_obj
        .and_then(|d| d.get("note"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let disposition_by = disposition_obj
        .and_then(|d| d.get("by"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let disposition_at = disposition_obj
        .and_then(|d| d.get("at"))
        .and_then(|v| v.as_i64());
    let resolution = f.get("resolution");
    let orphaned = resolution
        .and_then(|r| r.get("orphaned"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let confidence = resolution
        .and_then(|r| r.get("confidence"))
        .and_then(|v| v.as_str())
        .unwrap_or("exact")
        .to_string();
    let location_line = resolution
        .and_then(|r| r.get("line"))
        .and_then(|v| v.as_i64());
    let location = f.get("location");
    let location_path = location
        .and_then(|l| l.get("path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(FindingItem {
        review_id,
        slug,
        severity: f
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        category: f
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        location_path,
        location_line,
        title: f
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        author: f
            .get("author")
            .and_then(|v| v.as_str())
            .unwrap_or("claude")
            .to_string(),
        disposition,
        disposition_note,
        disposition_by,
        disposition_at,
        superseded: f
            .get("superseded")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        superseded_reason: f
            .get("superseded_reason")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        published_state: f
            .get("published_state")
            .and_then(|v| v.as_str())
            .unwrap_or("unpublished")
            .to_string(),
        orphaned,
        confidence,
        thread_count: f.get("thread_count").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
        unresolved_count: f
            .get("unresolved_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        updated_at: f.get("updated_at").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

/// Build a [`VerdictItem`] from `GET /api/reviews/{id}` (or the list
/// row of the same shape). `verdict: null` → state `"(none)"`.
pub fn verdict_from_review(body: &Value) -> Option<VerdictItem> {
    let review_id = body.get("id").and_then(|v| v.as_i64())?;
    let verdict = body.get("verdict");
    let (state, at, note) = match verdict {
        Some(v) if !v.is_null() => (
            v.get("state")
                .and_then(|s| s.as_str())
                .unwrap_or("(none)")
                .to_string(),
            v.get("at").and_then(|a| a.as_i64()).unwrap_or(0),
            v.get("note")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string()),
        ),
        _ => ("(none)".to_string(), 0, None),
    };
    Some(VerdictItem {
        review_id,
        state,
        stale: body
            .get("verdict_stale")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        at,
        note,
    })
}

/// Does this SSE event fall inside `scope`? Replay + live share this
/// filter so a ring-buffer replay of unrelated events is a no-op.
///
/// `review.changed{reason}` is open-ended (design doc §5); besides the
/// pre-existing `"verdict"` trigger, PRR-R6 additionally treats
/// `"findings_import"` / `"disposition"` / `"finding_published"` as
/// in-scope poll triggers — the actual findings refetch lives in
/// `fetch_scope`/`surface_scope` (same shape as every other trigger:
/// this function only gates WHETHER to refetch, never what).
fn is_findings_review_reason(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some("verdict") | Some("findings_import") | Some("disposition") | Some("finding_published")
    )
}

pub fn event_in_scope(event: &str, payload: &Value, scope: &WatchScope) -> bool {
    match event {
        "annotation.changed" | "suggestion.applied" => payload_matches_scope(payload, scope),
        "review.changed" => {
            is_findings_review_reason(payload.get("reason").and_then(|v| v.as_str()))
                && payload_matches_scope(payload, scope)
        }
        _ => false,
    }
}

fn payload_matches_scope(payload: &Value, scope: &WatchScope) -> bool {
    if let Some(want) = scope.review_id {
        match payload.get("review_id").and_then(|v| v.as_i64()) {
            Some(got) if got == want => {}
            _ => return false,
        }
    }
    if let Some(want) = scope.repo.as_deref() {
        match payload.get("repo").and_then(|v| v.as_str()) {
            Some(got) if got == want => {}
            _ => return false,
        }
    }
    true
}

/// Insert each item's key; return the ones that were new.
pub fn take_new(seen: &mut HashSet<SeenKey>, items: &[WatchItem]) -> Vec<WatchItem> {
    let mut out = Vec::new();
    for item in items {
        let key = item.seen_key();
        if seen.insert(key) {
            out.push(item.clone());
        }
    }
    out
}

/// Seed `seen` from `items`. When `backlog` is set, also return them
/// (so the caller can surface the initial open set once).
pub fn seed_seen(
    seen: &mut HashSet<SeenKey>,
    items: &[WatchItem],
    backlog: bool,
) -> Vec<WatchItem> {
    if backlog {
        take_new(seen, items)
    } else {
        for item in items {
            seen.insert(item.seen_key());
        }
        Vec::new()
    }
}

/// Drop items whose author is in `ignore` (echo-safety). Verdicts have
/// no author and always pass. Comparison is exact (the wire author
/// string, already lowercase-folded by the daemon).
pub fn filter_ignored<'a>(
    items: impl IntoIterator<Item = &'a WatchItem>,
    ignore: &[String],
) -> Vec<WatchItem> {
    items
        .into_iter()
        .filter(|item| match item.author() {
            Some(a) => !ignore.iter().any(|ig| ig == a),
            None => true,
        })
        .cloned()
        .collect()
}

/// First line of a body, trimmed, capped so the human line stays one row.
pub fn body_head(body: &str, max_chars: usize) -> String {
    let first = body.lines().next().unwrap_or("").trim();
    if first.chars().count() <= max_chars {
        first.to_string()
    } else {
        let truncated: String = first.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}…")
    }
}

/// Compact human line for one item.
pub fn format_human(item: &WatchItem) -> String {
    match item {
        WatchItem::Annotation(a) => {
            let loc = match a.line {
                Some(n) => format!("{}:{n}", a.path),
                None if a.orphaned => a.path.clone(),
                None => a.path.clone(),
            };
            let mut parts = vec![
                a.intent.clone(),
                loc,
                a.author.clone(),
                body_head(&a.body, 80),
            ];
            if a.orphaned {
                let was = match (a.original_ps, a.original_line) {
                    (Some(ps), Some(line)) => format!("⚠ orphaned (was ps{ps}:L{line})"),
                    _ => "⚠ orphaned".to_string(),
                };
                parts.insert(2, was);
            }
            if a.resolved {
                parts.push("resolved".to_string());
            }
            if a.suggestion_applied {
                parts.push("suggestion applied".to_string());
            } else if a.suggestion_pending {
                parts.push("suggestion pending".to_string());
            }
            if a.reply_count > 0 {
                parts.push(format!(
                    "{} {}",
                    a.reply_count,
                    if a.reply_count == 1 {
                        "reply"
                    } else {
                        "replies"
                    }
                ));
            }
            parts.join("  ")
        }
        WatchItem::Verdict(v) => {
            format!(
                "review {} verdict: {} (stale={})",
                v.review_id, v.state, v.stale
            )
        }
        WatchItem::Finding(f) => {
            let loc = match f.location_line {
                Some(n) => format!("{}:{n}", f.location_path),
                None => f.location_path.clone(),
            };
            let mut parts = vec![
                format!("finding {}", f.slug),
                f.severity.clone(),
                loc,
                f.author.clone(),
                body_head(&f.title, 80),
            ];
            if f.orphaned {
                parts.push("⚠ orphaned".to_string());
            }
            if let Some(d) = &f.disposition {
                let by = f.disposition_by.as_deref().unwrap_or("?");
                parts.push(format!("disposition={d}({by})"));
            }
            if f.superseded {
                parts.push("superseded".to_string());
            }
            if f.published_state != "unpublished" {
                parts.push(format!("published={}", f.published_state));
            }
            if f.unresolved_count > 0 {
                parts.push(format!("{} unresolved", f.unresolved_count));
            }
            parts.join("  ")
        }
    }
}

/// One `--json` line. Field names are the contract for LLM consumers.
pub fn format_json(item: &WatchItem) -> Value {
    match item {
        WatchItem::Annotation(a) => json!({
            "kind": "annotation",
            "id": a.id,
            "path": a.path,
            "line": a.line,
            "author": a.author,
            "intent": a.intent,
            "body": a.body,
            "resolved": a.resolved,
            "suggestion_applied": a.suggestion_applied,
            "suggestion_pending": a.suggestion_pending,
            "orphaned": a.orphaned,
            "original_ps": a.original_ps,
            "original_line": a.original_line,
            "reply_count": a.reply_count,
            "review_id": a.review_id,
            "updated_at": a.updated_at,
        }),
        WatchItem::Verdict(v) => json!({
            "kind": "verdict",
            "review_id": v.review_id,
            "state": v.state,
            "stale": v.stale,
            "at": v.at,
            "note": v.note,
        }),
        WatchItem::Finding(f) => json!({
            "kind": "finding",
            "review_id": f.review_id,
            "slug": f.slug,
            "severity": f.severity,
            "category": f.category,
            "path": f.location_path,
            "line": f.location_line,
            "title": f.title,
            "author": f.author,
            "disposition": f.disposition,
            "disposition_note": f.disposition_note,
            "disposition_by": f.disposition_by,
            "disposition_at": f.disposition_at,
            "superseded": f.superseded,
            "superseded_reason": f.superseded_reason,
            "published_state": f.published_state,
            "orphaned": f.orphaned,
            "confidence": f.confidence,
            "thread_count": f.thread_count,
            "unresolved_count": f.unresolved_count,
            "updated_at": f.updated_at,
        }),
    }
}

pub fn emit(item: &WatchItem, json: bool) {
    if json {
        println!("{}", format_json(item));
    } else {
        println!("{}", format_human(item));
    }
    let _ = std::io::stdout().flush();
}

/// Arguments for [`run`]. Named so the loop entry doesn't take a
/// 9-tuple.
pub struct WatchArgs {
    pub daemon: String,
    pub scope: WatchScope,
    pub json: bool,
    pub once: bool,
    pub timeout_secs: Option<u64>,
    pub backlog: bool,
    pub ignore_author: Vec<String>,
}

enum WatchExit {
    Done,
    StreamClosed,
}

pub async fn run(args: WatchArgs) -> Result<()> {
    let base = args.daemon.trim_end_matches('/').to_string();
    let scope_label = match (&args.scope.repo, args.scope.review_id) {
        (Some(r), Some(id)) => format!("repo={r} review={id}"),
        (Some(r), None) => format!("repo={r}"),
        (None, Some(id)) => format!("review={id}"),
        (None, None) => unreachable!("WatchScope::new rejects this"),
    };
    eprintln!("[kb-code annotate watch] watching {scope_label}");

    let mut seen: HashSet<SeenKey> = HashSet::new();
    let initial = fetch_scope(&base, &args.scope).await?;
    let mut surfaced = seed_seen(&mut seen, &initial, args.backlog);
    surfaced = filter_ignored(&surfaced, &args.ignore_author);
    for item in &surfaced {
        emit(item, args.json);
    }
    if args.once && !surfaced.is_empty() {
        return Ok(());
    }

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();

    let mut last_id: Option<String> = None;
    let mut step = 0usize;
    let mut deadline = args
        .timeout_secs
        .map(|s| Instant::now() + Duration::from_secs(s));

    loop {
        let resume = last_id.clone();
        let started = Instant::now();
        let outcome: Result<WatchExit> = tokio::select! {
            biased;
            _ = &mut ctrl_c => {
                eprintln!("[kb-code annotate watch] received SIGINT; exiting");
                return Ok(());
            }
            _ = async {
                #[cfg(unix)]
                { if let Some(s) = sigterm.as_mut() { s.recv().await; } else { std::future::pending::<()>().await; } }
                #[cfg(not(unix))]
                { std::future::pending::<()>().await; }
            } => {
                eprintln!("[kb-code annotate watch] received SIGTERM; exiting");
                return Ok(());
            }
            o = watch_once(
                &base,
                &args.scope,
                resume.as_deref(),
                &mut seen,
                &mut last_id,
                &mut deadline,
                &args,
            ) => o,
        };
        match outcome {
            Ok(WatchExit::Done) => return Ok(()),
            Ok(WatchExit::StreamClosed) => {
                let uptime = started.elapsed();
                if uptime >= STABLE_SESSION {
                    step = 0;
                    eprintln!("[kb-code annotate watch] stream closed; reconnecting in 1s …");
                } else {
                    let delay = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
                    eprintln!("[kb-code annotate watch] stream closed; retrying in {delay}s …");
                    step = (step + 1).min(BACKOFF_STEPS.len() - 1);
                }
            }
            Err(e) => {
                let delay = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
                eprintln!("[kb-code annotate watch] {e}; retrying in {delay}s …");
                step = (step + 1).min(BACKOFF_STEPS.len() - 1);
            }
        }
        let delay = BACKOFF_STEPS[step.min(BACKOFF_STEPS.len() - 1)];
        let sleep = tokio::time::sleep(Duration::from_secs(delay));
        tokio::pin!(sleep);
        tokio::select! {
            biased;
            _ = &mut ctrl_c => return Ok(()),
            _ = sleep_until_opt(deadline) => return Ok(()),
            _ = &mut sleep => {}
        }
    }
}

async fn watch_once(
    base: &str,
    scope: &WatchScope,
    last_event_id: Option<&str>,
    seen: &mut HashSet<SeenKey>,
    last_id: &mut Option<String>,
    deadline: &mut Option<Instant>,
    args: &WatchArgs,
) -> Result<WatchExit> {
    let mut session = sse::SseSession::connect(base, last_event_id).await?;

    // Rescan on (re)connect so a comment posted during a reconnect gap
    // is still surfaced. First connect finds nothing new (already seeded).
    if surface_scope(base, scope, seen, deadline, args).await? {
        return Ok(WatchExit::Done);
    }

    loop {
        let frame = tokio::select! {
            biased;
            _ = sleep_until_opt(*deadline) => return Ok(WatchExit::Done),
            f = session.next_frame() => f?,
        };
        let Some(frame) = frame else {
            *last_id = session.last_id().map(str::to_string);
            return Ok(WatchExit::StreamClosed);
        };
        if let Some(id) = &frame.id {
            *last_id = Some(id.clone());
        }
        if handle_frame(base, scope, seen, deadline, args, &frame).await? {
            return Ok(WatchExit::Done);
        }
    }
}

/// Returns `true` when `--once` is satisfied (caller should exit).
async fn handle_frame(
    base: &str,
    scope: &WatchScope,
    seen: &mut HashSet<SeenKey>,
    deadline: &mut Option<Instant>,
    args: &WatchArgs,
    frame: &Frame,
) -> Result<bool> {
    let Some(kind) = frame.event.as_deref() else {
        return Ok(false);
    };
    let raw = frame.data.as_deref().unwrap_or("{}");
    let envelope: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[kb-code annotate watch] malformed {kind} payload: {e}");
            return Ok(false);
        }
    };
    let payload = envelope.get("payload").unwrap_or(&envelope);
    if !event_in_scope(kind, payload, scope) {
        return Ok(false);
    }
    if kind == "review.changed" {
        if let Some(rid) = payload.get("review_id").and_then(|v| v.as_i64()) {
            if surface_verdict(base, rid, seen, deadline, args).await? {
                return Ok(true);
            }
        }
    }
    surface_scope(base, scope, seen, deadline, args).await
}

async fn surface_scope(
    base: &str,
    scope: &WatchScope,
    seen: &mut HashSet<SeenKey>,
    deadline: &mut Option<Instant>,
    args: &WatchArgs,
) -> Result<bool> {
    let items = fetch_scope(base, scope).await?;
    let fresh = take_new(seen, &items);
    let fresh = filter_ignored(&fresh, &args.ignore_author);
    for item in &fresh {
        emit(item, args.json);
        reset_idle(deadline, args.timeout_secs);
    }
    Ok(args.once && !fresh.is_empty())
}

async fn surface_verdict(
    base: &str,
    review_id: i64,
    seen: &mut HashSet<SeenKey>,
    deadline: &mut Option<Instant>,
    args: &WatchArgs,
) -> Result<bool> {
    let body = match fetch_review(base, review_id).await? {
        Some(b) => b,
        None => return Ok(false),
    };
    let Some(item) = verdict_from_review(&body) else {
        return Ok(false);
    };
    let item = WatchItem::Verdict(item);
    let fresh = take_new(seen, std::slice::from_ref(&item));
    let fresh = filter_ignored(&fresh, &args.ignore_author);
    for it in &fresh {
        emit(it, args.json);
        reset_idle(deadline, args.timeout_secs);
    }
    Ok(args.once && !fresh.is_empty())
}

fn reset_idle(deadline: &mut Option<Instant>, timeout_secs: Option<u64>) {
    if let Some(s) = timeout_secs {
        *deadline = Some(Instant::now() + Duration::from_secs(s));
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending::<()>().await,
    }
}

async fn fetch_scope(base: &str, scope: &WatchScope) -> Result<Vec<WatchItem>> {
    if let Some(id) = scope.review_id {
        let comments = get_json(
            base,
            &format!("/api/reviews/{id}/comments"),
            &[("all", "true")],
        )
        .await?;
        let mut items = items_from_comments(&comments);
        // PRR-R6 (design doc §3.3): findings are review-scoped only (no
        // repo-wide findings route exists), so this poll only fires
        // when the scope carries a review id — the same condition that
        // already gates the `/comments` poll above.
        // `include_superseded=true` so a supersession surfaces once
        // rather than silently dropping out of the polled set.
        let findings = get_json(
            base,
            &format!("/api/reviews/{id}/findings"),
            &[("include_superseded", "true")],
        )
        .await?;
        items.extend(items_from_findings(&findings));
        return Ok(items);
    }
    let repo = scope
        .repo
        .as_deref()
        .expect("WatchScope::new requires repo or review");
    let body = get_json(base, "/api/annotations/open", &[("repo", repo)]).await?;
    Ok(items_from_open(&body))
}

async fn fetch_review(base: &str, id: i64) -> Result<Option<Value>> {
    let url = format!("{base}/api/reviews/{id}");
    let client = crate::client_builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if resp.status().as_u16() == 404 {
        return Ok(None);
    }
    let body = resp
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await
        .with_context(|| format!("parse {url}"))?;
    Ok(Some(body))
}

async fn get_json(base: &str, path: &str, query: &[(&str, &str)]) -> Result<Value> {
    let url = format!("{base}{path}");
    let client = crate::client_builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let resp = client
        .get(&url)
        .query(query)
        .send()
        .await
        .with_context(|| format!("GET {url} — is kb-code-server running at {base}?"))?;
    resp.error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ann(id: &str, updated_at: i64, resolved: bool, applied: bool) -> WatchItem {
        WatchItem::Annotation(AnnotationItem {
            id: id.into(),
            path: "lib.rs".into(),
            line: Some(2),
            author: "you".into(),
            intent: "note".into(),
            body: "hello\nworld".into(),
            resolved,
            suggestion_applied: applied,
            suggestion_pending: false,
            orphaned: false,
            original_ps: None,
            original_line: None,
            reply_count: 0,
            review_id: None,
            updated_at,
        })
    }

    /// PRR-R6 test fixture — a `FindingItem` with every dedup-relevant
    /// field parameterized; unrelated fields (category/title/location)
    /// are held fixed across the tests below.
    #[allow(clippy::too_many_arguments)]
    fn finding(
        slug: &str,
        updated_at: i64,
        disposition: Option<&str>,
        disposition_by: Option<&str>,
        superseded: bool,
        published_state: &str,
        author: &str,
    ) -> WatchItem {
        WatchItem::Finding(FindingItem {
            review_id: 7,
            slug: slug.into(),
            severity: "concern".into(),
            category: "Concurrency".into(),
            location_path: "app/models/order.rb".into(),
            location_line: Some(88),
            title: "Duplicate order rows possible under concurrent checkout".into(),
            author: author.into(),
            disposition: disposition.map(|s| s.to_string()),
            disposition_note: None,
            disposition_by: disposition_by.map(|s| s.to_string()),
            disposition_at: disposition.map(|_| 100),
            superseded,
            superseded_reason: if superseded {
                Some("not_in_reimport".into())
            } else {
                None
            },
            published_state: published_state.into(),
            orphaned: false,
            confidence: "exact".into(),
            thread_count: 0,
            unresolved_count: 0,
            updated_at,
        })
    }

    #[test]
    fn seed_without_backlog_does_not_surface() {
        let mut seen = HashSet::new();
        let items = vec![ann("a1", 1, false, false)];
        let surfaced = seed_seen(&mut seen, &items, false);
        assert!(surfaced.is_empty());
        assert_eq!(seen.len(), 1);
        // A later identical refetch finds nothing new.
        assert!(take_new(&mut seen, &items).is_empty());
    }

    #[test]
    fn seed_with_backlog_surfaces_once() {
        let mut seen = HashSet::new();
        let items = vec![ann("a1", 1, false, false)];
        let first = seed_seen(&mut seen, &items, true);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].seen_key(), items[0].seen_key());
        assert!(take_new(&mut seen, &items).is_empty());
    }

    #[test]
    fn key_changes_on_updated_resolved_or_applied() {
        let mut seen = HashSet::new();
        let a = ann("a1", 1, false, false);
        assert_eq!(take_new(&mut seen, std::slice::from_ref(&a)).len(), 1);
        assert!(take_new(&mut seen, std::slice::from_ref(&a)).is_empty());
        assert_eq!(take_new(&mut seen, &[ann("a1", 2, false, false)]).len(), 1);
        assert_eq!(take_new(&mut seen, &[ann("a1", 2, true, false)]).len(), 1);
        assert_eq!(take_new(&mut seen, &[ann("a1", 2, true, true)]).len(), 1);
    }

    #[test]
    fn replay_refetch_is_idempotent() {
        // Cold connect replays the ring; those events trigger a refetch
        // that sees the already-seeded set. Nothing new must surface.
        let mut seen = HashSet::new();
        let items = vec![ann("a1", 10, false, false), ann("a2", 11, false, true)];
        let _ = seed_seen(&mut seen, &items, false);
        assert!(take_new(&mut seen, &items).is_empty());
    }

    #[test]
    fn ignore_author_skips_emit_but_verdicts_pass() {
        let you = ann("a1", 1, false, false);
        let mut claude = match ann("a2", 1, false, false) {
            WatchItem::Annotation(mut a) => {
                a.author = "claude".into();
                WatchItem::Annotation(a)
            }
            other => other,
        };
        if let WatchItem::Annotation(ref mut a) = claude {
            a.id = "a2".into();
        }
        let verdict = WatchItem::Verdict(VerdictItem {
            review_id: 7,
            state: "approve".into(),
            stale: false,
            at: 1,
            note: None,
        });
        let ignore = vec!["claude".to_string()];
        let kept = filter_ignored([&you, &claude, &verdict], &ignore);
        assert_eq!(kept.len(), 2);
        assert!(matches!(&kept[0], WatchItem::Annotation(a) if a.author == "you"));
        assert!(matches!(&kept[1], WatchItem::Verdict(_)));
    }

    #[test]
    fn items_from_open_and_comments() {
        let open = json!({
            "annotations": [
                {
                    "id": "ann_1",
                    "path": "src/a.rs",
                    "line": 4,
                    "author": "you",
                    "intent": "question",
                    "body": "why?",
                    "resolved": false,
                    "updated_at": 42,
                    "reply_count": 2,
                    "review_id": 3
                }
            ]
        });
        let items = items_from_open(&open);
        assert_eq!(items.len(), 1);
        match &items[0] {
            WatchItem::Annotation(a) => {
                assert_eq!(a.id, "ann_1");
                assert_eq!(a.line, Some(4));
                assert_eq!(a.reply_count, 2);
                assert!(!a.suggestion_applied);
            }
            WatchItem::Verdict(_) => panic!("expected annotation"),
            WatchItem::Finding(_) => panic!("expected annotation"),
        }

        let comments = json!({
            "schema": "review-comments/1",
            "review_id": 9,
            "groups": [{
                "path": "lib.rs",
                "comments": [{
                    "id": "ann_c",
                    "path": "lib.rs",
                    "intent": "note",
                    "body": "fix this",
                    "author": "you",
                    "updated_at": 7,
                    "resolved": false,
                    "resolution": {
                        "line": 12,
                        "line_end": null,
                        "orphaned": false,
                        "resolved_against": {"ps": 2, "sha": "abc"}
                    },
                    "suggestion": {
                        "replacement": "x",
                        "original": "y",
                        "applied": false,
                        "applied_at": null
                    },
                    "replies": [{"id": "r1"}]
                }, {
                    "id": "ann_orphan",
                    "path": "lib.rs",
                    "intent": "flag-for-agent",
                    "body": "gone",
                    "author": "you",
                    "updated_at": 8,
                    "resolved": false,
                    "resolution": {
                        "line": null,
                        "orphaned": true,
                        "original": {"ps": 1, "side": "new", "line": 10, "snippet": "old"}
                    },
                    "suggestion": null,
                    "replies": []
                }]
            }]
        });
        let items = items_from_comments(&comments);
        assert_eq!(items.len(), 2);
        match &items[0] {
            WatchItem::Annotation(a) => {
                assert_eq!(a.line, Some(12));
                assert!(a.suggestion_pending);
                assert!(!a.suggestion_applied);
                assert_eq!(a.reply_count, 1);
                assert_eq!(a.review_id, Some(9));
            }
            WatchItem::Verdict(_) => panic!("expected annotation"),
            WatchItem::Finding(_) => panic!("expected annotation"),
        }
        match &items[1] {
            WatchItem::Annotation(a) => {
                assert!(a.orphaned);
                assert_eq!(a.original_ps, Some(1));
                assert_eq!(a.original_line, Some(10));
            }
            WatchItem::Verdict(_) => panic!("expected annotation"),
            WatchItem::Finding(_) => panic!("expected annotation"),
        }
    }

    #[test]
    fn event_in_scope_filters_type_and_ids() {
        let repo = WatchScope {
            repo: Some("kb".into()),
            review_id: None,
        };
        let review = WatchScope {
            repo: None,
            review_id: Some(4),
        };
        let both = WatchScope {
            repo: Some("kb".into()),
            review_id: Some(4),
        };
        let ann = json!({"repo": "kb", "path": "a.rs", "review_id": 4});
        let other_repo = json!({"repo": "other", "path": "a.rs"});
        let verdict = json!({"repo": "kb", "review_id": 4, "reason": "verdict"});
        let patchset = json!({"repo": "kb", "review_id": 4, "reason": "patchset"});
        let findings_import = json!({"repo": "kb", "review_id": 4, "reason": "findings_import"});
        let disposition =
            json!({"repo": "kb", "review_id": 4, "reason": "disposition", "finding_slug": "f-x"});
        let finding_published = json!({"repo": "kb", "review_id": 4, "reason": "finding_published", "finding_slug": "f-x"});

        assert!(event_in_scope("annotation.changed", &ann, &repo));
        assert!(event_in_scope("suggestion.applied", &ann, &review));
        assert!(!event_in_scope("annotation.changed", &other_repo, &repo));
        assert!(!event_in_scope("annotation.changed", &other_repo, &review));
        assert!(event_in_scope("review.changed", &verdict, &both));
        assert!(!event_in_scope("review.changed", &patchset, &both));
        assert!(!event_in_scope("mirror.updated", &ann, &repo));

        // PRR-R6 (design doc §5): findings_import/disposition/
        // finding_published are ALSO poll triggers for a watched
        // review, same as the pre-existing verdict trigger.
        assert!(event_in_scope("review.changed", &findings_import, &both));
        assert!(event_in_scope("review.changed", &disposition, &both));
        assert!(event_in_scope("review.changed", &finding_published, &both));
        assert!(event_in_scope("review.changed", &findings_import, &review));
        assert!(!event_in_scope(
            "review.changed",
            &json!({"repo": "other", "review_id": 4, "reason": "findings_import"}),
            &repo
        ));
    }

    // --- PRR-R6: finding SeenKey / author / parsing ---------------------

    #[test]
    fn items_from_findings_parses_the_review_findings_1_wire_shape() {
        let body = json!({
            "schema": "review-findings/1",
            "review_id": 7,
            "repo": "kb",
            "ps": 2,
            "findings": [{
                "slug": "f-dedup-race",
                "severity": "concern",
                "category": "Concurrency",
                "location": {"kind": "range", "path": "app/models/order.rb", "lines": [88, 102], "removed": false},
                "title": "Duplicate order rows possible",
                "rationale": "no unique index",
                "recommendation": "add one",
                "evidence": null,
                "origin": "import",
                "author": "claude",
                "disposition": null,
                "published_state": "unpublished",
                "published_at": null,
                "published_url": null,
                "superseded": false,
                "superseded_reason": null,
                "content_updated_at": null,
                "annotation_id": "ann_1",
                "import_batch_id": "batch_abc",
                "created_at": 1,
                "updated_at": 5,
                "resolution": {"line": 88, "line_end": 102, "orphaned": false, "confidence": "exact"},
                "thread_count": 0,
                "unresolved_count": 0
            }, {
                "slug": "f-disposed",
                "severity": "blocker",
                "category": "Security",
                "location": {"kind": "single", "path": "app/x.rb", "lines": [1], "removed": false},
                "title": "unsafe eval",
                "rationale": "r",
                "recommendation": null,
                "evidence": null,
                "origin": "import",
                "author": "claude",
                "disposition": {"state": "agree", "note": "yep", "by": "carol", "at": 99},
                "published_state": "published",
                "published_at": 100,
                "published_url": "https://github.com/o/r/pull/1#comment",
                "superseded": true,
                "superseded_reason": "not_in_reimport",
                "content_updated_at": 3,
                "annotation_id": "ann_2",
                "import_batch_id": "batch_abc",
                "created_at": 1,
                "updated_at": 9,
                "resolution": {"line": null, "line_end": null, "orphaned": true, "confidence": "orphaned"},
                "thread_count": 2,
                "unresolved_count": 1
            }]
        });
        let items = items_from_findings(&body);
        assert_eq!(items.len(), 2);
        match &items[0] {
            WatchItem::Finding(f) => {
                assert_eq!(f.review_id, 7);
                assert_eq!(f.slug, "f-dedup-race");
                assert_eq!(f.location_path, "app/models/order.rb");
                assert_eq!(f.location_line, Some(88));
                assert_eq!(f.author, "claude");
                assert!(f.disposition.is_none());
                assert!(!f.superseded);
                assert_eq!(f.published_state, "unpublished");
                assert_eq!(f.updated_at, 5);
            }
            other => panic!("expected finding, got {other:?}"),
        }
        match &items[1] {
            WatchItem::Finding(f) => {
                assert_eq!(f.disposition.as_deref(), Some("agree"));
                assert_eq!(f.disposition_by.as_deref(), Some("carol"));
                assert_eq!(f.disposition_note.as_deref(), Some("yep"));
                assert_eq!(f.disposition_at, Some(99));
                assert!(f.superseded);
                assert_eq!(f.superseded_reason.as_deref(), Some("not_in_reimport"));
                assert_eq!(f.published_state, "published");
                assert!(f.orphaned);
                assert_eq!(f.confidence, "orphaned");
                assert_eq!(f.thread_count, 2);
                assert_eq!(f.unresolved_count, 1);
            }
            other => panic!("expected finding, got {other:?}"),
        }
    }

    #[test]
    fn new_finding_surfaces_once() {
        let mut seen = HashSet::new();
        let f = finding(
            "f-dedup-race",
            1,
            None,
            None,
            false,
            "unpublished",
            "claude",
        );
        assert_eq!(take_new(&mut seen, std::slice::from_ref(&f)).len(), 1);
        assert!(take_new(&mut seen, std::slice::from_ref(&f)).is_empty());
    }

    #[test]
    fn disposition_change_surfaces_once_and_author_is_the_disposer() {
        let mut seen = HashSet::new();
        let before = finding("f-x", 1, None, None, false, "unpublished", "claude");
        let _ = seed_seen(&mut seen, std::slice::from_ref(&before), false);

        // A human sets a disposition — updated_at + disposition both
        // change together, as the real route does.
        let after = finding(
            "f-x",
            2,
            Some("agree"),
            Some("carol"),
            false,
            "unpublished",
            "claude",
        );
        let fresh = take_new(&mut seen, std::slice::from_ref(&after));
        assert_eq!(fresh.len(), 1);
        assert_eq!(fresh[0].author(), Some("carol"));
        // Re-polling the same disposed state doesn't resurface.
        assert!(take_new(&mut seen, std::slice::from_ref(&after)).is_empty());
    }

    #[test]
    fn supersede_surfaces_once() {
        let mut seen = HashSet::new();
        let before = finding("f-y", 1, None, None, false, "unpublished", "claude");
        let _ = seed_seen(&mut seen, std::slice::from_ref(&before), false);

        let after = finding("f-y", 2, None, None, true, "unpublished", "claude");
        assert_eq!(take_new(&mut seen, std::slice::from_ref(&after)).len(), 1);
        assert!(take_new(&mut seen, std::slice::from_ref(&after)).is_empty());
    }

    #[test]
    fn unchanged_publish_state_does_not_resurface_but_a_real_change_does() {
        let mut seen = HashSet::new();
        let f = finding("f-z", 1, None, None, false, "unpublished", "claude");
        let _ = seed_seen(&mut seen, std::slice::from_ref(&f), false);

        // Byte-identical republish (nothing changed) — must not resurface.
        assert!(take_new(&mut seen, std::slice::from_ref(&f)).is_empty());

        // An actual publish-state transition IS a new surfaceable item.
        let published = finding("f-z", 2, None, None, false, "published", "claude");
        assert_eq!(
            take_new(&mut seen, std::slice::from_ref(&published)).len(),
            1
        );
        assert!(take_new(&mut seen, std::slice::from_ref(&published)).is_empty());
    }

    #[test]
    fn ignore_author_claude_suppresses_import_but_not_a_human_disposition() {
        // A fresh import, never disposed — author() falls back to the
        // finding's own `author` field ("claude").
        let import_only = finding("f-w", 1, None, None, false, "unpublished", "claude");
        // A human disposition on the SAME import-origin finding —
        // author() reads disposition_by instead.
        let human_disposition = finding(
            "f-w",
            2,
            Some("agree"),
            Some("you"),
            false,
            "unpublished",
            "claude",
        );

        let ignore = vec!["claude".to_string()];
        assert!(filter_ignored([&import_only], &ignore).is_empty());
        let kept = filter_ignored([&human_disposition], &ignore);
        assert_eq!(kept.len(), 1);
        assert!(
            matches!(&kept[0], WatchItem::Finding(f) if f.disposition_by.as_deref() == Some("you"))
        );
    }

    #[test]
    fn finding_format_human_and_json_shapes() {
        let f = finding(
            "f-dedup-race",
            5,
            Some("agree"),
            Some("carol"),
            false,
            "unpublished",
            "claude",
        );
        let line = format_human(&f);
        assert!(line.contains("finding f-dedup-race"));
        assert!(line.contains("concern"));
        assert!(line.contains("app/models/order.rb:88"));
        assert!(line.contains("claude"));
        assert!(line.contains("disposition=agree(carol)"));

        let j = format_json(&f);
        assert_eq!(j["kind"], "finding");
        assert_eq!(j["slug"], "f-dedup-race");
        assert_eq!(j["review_id"], 7);
        assert_eq!(j["disposition"], "agree");
        assert_eq!(j["disposition_by"], "carol");
        assert_eq!(j["superseded"], false);
        assert_eq!(j["published_state"], "unpublished");

        let superseded = finding("f-old", 1, None, None, true, "unpublished", "claude");
        let sline = format_human(&superseded);
        assert!(sline.contains("superseded"));

        let published = finding("f-pub", 1, None, None, false, "published", "claude");
        assert!(format_human(&published).contains("published=published"));
    }

    #[test]
    fn format_human_and_json_shapes() {
        let a = WatchItem::Annotation(AnnotationItem {
            id: "ann_x".into(),
            path: "lib.rs".into(),
            line: Some(2),
            author: "you".into(),
            intent: "question".into(),
            body: "why is this here?\nmore".into(),
            resolved: false,
            suggestion_applied: true,
            suggestion_pending: false,
            orphaned: false,
            original_ps: None,
            original_line: None,
            reply_count: 1,
            review_id: Some(3),
            updated_at: 9,
        });
        let line = format_human(&a);
        assert!(line.contains("question"));
        assert!(line.contains("lib.rs:2"));
        assert!(line.contains("you"));
        assert!(line.contains("why is this here?"));
        assert!(line.contains("suggestion applied"));
        assert!(line.contains("1 reply"));
        let j = format_json(&a);
        assert_eq!(j["kind"], "annotation");
        assert_eq!(j["id"], "ann_x");
        assert_eq!(j["suggestion_applied"], true);
        assert_eq!(j["review_id"], 3);

        let orphan = WatchItem::Annotation(AnnotationItem {
            id: "ann_o".into(),
            path: "lib.rs".into(),
            line: None,
            author: "you".into(),
            intent: "note".into(),
            body: "gone".into(),
            resolved: true,
            suggestion_applied: false,
            suggestion_pending: false,
            orphaned: true,
            original_ps: Some(1),
            original_line: Some(10),
            reply_count: 0,
            review_id: None,
            updated_at: 1,
        });
        let oline = format_human(&orphan);
        assert!(oline.contains("⚠ orphaned (was ps1:L10)"));
        assert!(oline.contains("resolved"));

        let v = WatchItem::Verdict(VerdictItem {
            review_id: 4,
            state: "approve".into(),
            stale: false,
            at: 11,
            note: Some("lgtm".into()),
        });
        assert_eq!(format_human(&v), "review 4 verdict: approve (stale=false)");
        let jv = format_json(&v);
        assert_eq!(jv["kind"], "verdict");
        assert_eq!(jv["state"], "approve");
        assert_eq!(jv["stale"], false);
    }

    #[test]
    fn verdict_from_review_handles_null_and_set() {
        let none = json!({"id": 1, "verdict": null, "verdict_stale": false});
        let v = verdict_from_review(&none).unwrap();
        assert_eq!(v.state, "(none)");
        assert!(!v.stale);

        let set = json!({
            "id": 2,
            "verdict": {"state": "request-changes", "note": "n", "at": 99, "ps": 1},
            "verdict_stale": true
        });
        let v = verdict_from_review(&set).unwrap();
        assert_eq!(v.state, "request-changes");
        assert!(v.stale);
        assert_eq!(v.at, 99);
        assert_eq!(v.note.as_deref(), Some("n"));
    }

    #[test]
    fn watch_scope_requires_repo_or_review() {
        assert!(WatchScope::new(None, None).is_err());
        assert!(WatchScope::new(Some("r".into()), None).is_ok());
        assert!(WatchScope::new(None, Some(1)).is_ok());
    }
}
