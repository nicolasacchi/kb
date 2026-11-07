//! A3 — `kb-session-card/1`: one daemon-rendered read model for a session.
//!
//! The card is a SURFACED, NEVER-SCORED derived read over other lanes'
//! readers (invariant #11 amendment "SESSION CARD (A3)"): nothing in a
//! ranking module reads it, and the lineage columns it carries are
//! display-only. Pure types + the pure text renderer live here; the fan-out
//! that fills them lives in `kb_server::routes::session_card`. Lane
//! payloads never include transcript BODY or raw turns (stored, capped,
//! derived facts only — the outcome preview, display names, stored
//! decisions, commit subjects). Every list has an explicit total order;
//! no `HashMap` iteration reaches output.

use crate::sessions::live::{Confidence, Holder, LiveState};
use serde::Serialize;
use std::collections::BTreeMap;

/// Wire schema string of the card (`SessionCard.schema`).
pub const SESSION_CARD_SCHEMA: &str = "kb-session-card/1";

/// `not_committed` considers at most this many of a session's resolved
/// `kind="commit"` commits (newest first by `seq`).
pub const NOT_COMMITTED_COMMIT_CAP: usize = 20;
/// The renderer's line budget: `render_text` never emits a longer line.
pub const CARD_TEXT_LINE_MAX_CHARS: usize = 80;

// ---------------------------------------------------------------------------
// Pure helpers (set difference, captions, sanitising) — everything pinned
// by unit tests below.
// ---------------------------------------------------------------------------

/// Drop ALL control characters (including `\x1b` and `\r`) so a
/// transcript-derived title can never inject terminal escapes into the
/// operator's terminal via the CLI, then truncate by chars with the same
/// ellipsis the rest of the sessions wire uses.
pub fn sanitize_line(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    crate::sessions::truncate_chars_ellipsis(&clean, CARD_TEXT_LINE_MAX_CHARS)
}

/// Pure set difference for `not_committed`: the edited/wrote paths minus
/// the union of the considered commits' touched files, de-duplicated and
/// sorted for determinism (BTreeMap-free: a sorted `Vec`).
pub fn not_committed_diff(edited: &[String], touched: &[String]) -> Vec<String> {
    let touched: std::collections::HashSet<&str> = touched.iter().map(String::as_str).collect();
    let mut out: Vec<String> = edited
        .iter()
        .filter(|p| !touched.contains(p.as_str()))
        .cloned()
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The caption for a partial `not_committed` coverage: `Some` whenever not
/// every resolved `kind="commit"` row was considered (`capped`) or any
/// commit row was unusable (`unresolved > 0`). Never returns a caption for
/// full coverage, so a complete check stays silent. A partial caption is
/// mandatory before a non-empty `not_committed` list may be emitted — the
/// card's outcome-lane builder enforces that (see
/// `session_card::outcome_lane`).
pub fn not_committed_caption(
    commits_total: u32,
    considered: u32,
    unresolved: u32,
) -> Option<String> {
    let capped = considered < commits_total;
    if !capped && unresolved == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if capped {
        parts.push(format!(
            "checked the newest {considered} of {commits_total} commits; files may be committed earlier"
        ));
    }
    if unresolved > 0 {
        parts.push(format!(
            "{unresolved} unresolved commit(s) not checked; files may be committed earlier"
        ));
    }
    Some(parts.join("; "))
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionCard {
    /// Always `"kb-session-card/1"` ([`SESSION_CARD_SCHEMA`]).
    pub schema: String,
    pub session_id: String,
    /// The kb of the session's newest capture.
    pub kb: String,
    /// The newest capture's artifact id (`SessionOut.artifact_id`).
    pub artifact_id: String,
    /// The daemon-rendered plain text (see [`render_text`]).
    pub text: String,
    pub summary: SummaryLane,
    pub outcome: OutcomeLane,
    pub files: FilesLane,
    pub decisions: DecisionsLane,
    pub commits: CommitsLane,
    pub research: ResearchLane,
    pub comments: CommentsLane,
    pub readings: ReadingsLane,
    pub memory: MemoryLane,
    pub live: LiveLane,
    pub slate: SlateLane,
    pub children: ChildrenLane,
    /// Constant until the kb-term track fills it; the card gives agents
    /// and the CLI no attach or type surface.
    pub terminal: TerminalLane,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SummaryLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub title: Option<String>,
    pub display_name: String,
    pub harness: String,
    pub project_slug: Option<String>,
    pub substance: Option<String>,
    pub active_secs: i64,
    pub started_at: i64,
    pub ended_at: i64,
    /// Every chain part's session id, part 1 first; empty unless the
    /// session is a segmented chain of >= 2 parts.
    pub segment_chain: Vec<String>,
    /// V0047 lineage (surfaced, never scored); `None` for top-level rows.
    pub root_session_id: Option<String>,
    pub parent_session_id: Option<String>,
    /// How many newest-capture rows share this tree root (the same count
    /// the children lane reports as its total).
    pub child_total: u64,
    /// Root-targeted resume command (the root's id, a segment part's
    /// chain head, else the row's own id — the same rule `SessionOut`
    /// uses).
    pub resume: String,
}

impl SummaryLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutcomeLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The session's closing assistant text, server-capped
    /// (`OUTCOME_WIRE_MAX_CHARS`); `None` for a prose-less capture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_text: Option<String>,
    /// V0046 evidence exactly as `GET /api/sessions/{id}` emits it: the
    /// verdict and every kept check mapped through the same `check_out`.
    pub verdict: crate::sessions::checks::Verdict,
    pub checks: Vec<CardCheckOut>,
    /// `None` when the files/commits lanes failed and no honest diff can
    /// be computed; otherwise the (possibly empty) difference set.
    pub not_committed: Option<NotCommitted>,
}

impl OutcomeLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            last_assistant_text: None,
            verdict: crate::sessions::checks::Verdict::NotExtracted,
            checks: Vec::new(),
            not_committed: None,
        }
    }
}

/// One check run, as the wire and every presenter show it (the
/// `kb-server` `check_out` mapping, field for field).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CardCheckOut {
    pub label: String,
    pub cmd: String,
    pub runners: Vec<String>,
    pub outcome: crate::sessions::checks::Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<crate::sessions::checks::Basis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<crate::sessions::checks::Why>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_last_edit: Option<bool>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NotCommitted {
    /// Edited/wrote paths no considered commit touched, sorted.
    pub files: Vec<String>,
    /// How many resolved commits were consulted (`<= 20`).
    pub commits_considered: u32,
    /// All `kind="commit"` rows of the capture.
    pub commits_total: u32,
    /// The considered set was cut to the newest 20.
    pub capped: bool,
    /// `kind="commit"` rows not usable by the diff (unresolved, or a
    /// resolved row whose git resolution metadata is missing).
    pub unresolved_commits: u32,
    /// Present whenever coverage is partial — computed by
    /// [`not_committed_caption`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FileItem {
    pub path: String,
    /// `"read" | "write" | "edit"`.
    pub action: String,
    /// V0026 — recovered from a subagent's sidecar transcript.
    pub via_subagent: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct FilesLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Newest-capture file rows sorted by path, first 100.
    pub files: Vec<FileItem>,
    /// Every file row of the capture, not just the emitted window.
    pub total: u32,
    pub read: u32,
    pub edited: u32,
    pub wrote: u32,
}

impl FilesLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DecisionItem {
    pub kind: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DecisionsLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// First 50 decisions by `seq`.
    pub decisions: Vec<DecisionItem>,
    pub total: u32,
}

impl DecisionsLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CommitItem {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub resolved: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CommitsLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// First 50 commits by `seq`.
    pub commits: Vec<CommitItem>,
    pub total: u32,
}

impl CommitsLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ResearchItem {
    pub kind: String,
    pub query: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ResearchLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Research-signal counts by `kind` (`kb_search | web | skill |
    /// subagent | plan_span | artifact_open`).
    pub kind_counts: BTreeMap<String, u32>,
    /// First 20 rows by `seq`.
    pub items: Vec<ResearchItem>,
    pub total: u32,
}

impl ResearchLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CommentItem {
    pub comment_id: String,
    pub kb: String,
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub author: String,
    pub body: String,
    pub file_label: String,
    pub created_at: i64,
    /// `"open" | "resolved"`.
    pub status: String,
    /// Also an artifact the session touched in-corpus.
    pub touched: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CommentsLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Open comments on in-corpus artifacts the session touched.
    pub open_count: u32,
    /// Comments raised during the session's window.
    pub raised_count: u32,
    /// First 20 open items, in the handler's (kb, artifact) order.
    pub open: Vec<CommentItem>,
    /// First 20 raised items, newest first.
    pub raised: Vec<CommentItem>,
}

impl CommentsLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReadingItem {
    pub kb: String,
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_relative: Option<String>,
    /// Unix seconds of the (newest-in-window) open.
    pub opened_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_section: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ReadingsLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// First 20 readings, newest open first.
    pub items: Vec<ReadingItem>,
    pub total: u32,
}

impl ReadingsLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RecallItem {
    pub kb: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_relative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recalled_at: Option<i64>,
    pub used: bool,
    /// MR1 rank in the injection pack that served the hit (`None` on a
    /// pre-MR1 capture or an unparsable marker — never inferred).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MemoryLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Memory ids stamped with this session (the same fan-out
    /// `GET /api/sessions/{id}` uses), sorted+deduped, first 50.
    pub produced: Vec<String>,
    /// Every produced id, not just the emitted window.
    pub produced_total: u32,
    /// Every memory this session's recall hook injected.
    pub recalled: Vec<RecallItem>,
}

impl MemoryLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LiveLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The two-axis live vocabulary (`working | stalled | waiting | cold |
    /// finished | presumed_ended`), shared with live-status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<LiveState>,
    /// `agent | human | none`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<Holder>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    /// Seconds since the registry's last activity, `max(0, …)` ten-clamped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_secs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<bool>,
}

impl LiveLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SlatePost {
    pub seq: u64,
    pub id: String,
    pub at: i64,
    /// The post's `Kind` wire spelling ("now" | "warn" | "take" | …).
    pub kind: String,
    pub line: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SlateLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_seq: Option<u64>,
    /// The newest 20 matching posts (no body).
    pub posts: Vec<SlatePost>,
    /// Every matching post, not just the emitted window.
    pub total: u32,
}

impl SlateLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ChildRow {
    pub session_id: String,
    pub display_name: String,
    pub started_at: i64,
    pub ended_at: i64,
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub substance: Option<String>,
    pub files_edited_count: u32,
    pub tool_calls: u32,
    /// V0047 lineage (surfaced, never scored): the tree root's session id
    /// (absent on the root's own rows), repeated per child for tree math.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ChildrenLane {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Every child of the target session (the whole flood for a root,
    /// direct children only for a non-root).
    pub total: u64,
    /// First 50 by `(started_at ASC, session_id ASC)`.
    pub items: Vec<ChildRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_after: Option<String>,
}

impl ChildrenLane {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

/// The constant terminal lane: always unavailable until the kb-term track
/// fills it from the live registry. `available` and `reason` are
/// identical on every read, on every config — there is no `[terminal]`
/// key to consult and no `term_id`/`admitted` to surface.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TerminalLane {
    pub available: bool,
    pub reason: String,
}

impl TerminalLane {
    pub fn constant() -> Self {
        Self {
            available: false,
            reason: "no terminal runner".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// `text` — the daemon-rendered plain text the CLI prints verbatim.
// ---------------------------------------------------------------------------

fn fmt_opt(v: &Option<String>) -> String {
    v.clone().unwrap_or_else(|| "—".to_string())
}

/// Section body for an available lane; `None` for an unavailable lane (the
/// caller renders the one-line `unavailable (<reason>)` form instead).
fn render_section_lines(card: &SessionCard) -> Vec<(String, Vec<String>)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();

    let s = &card.summary;
    if s.available {
        let mut lines = vec![
            format!("session {}", sanitize_line(&card.session_id)),
            sanitize_line(&format!("{} ({})", s.display_name, s.harness)),
            sanitize_line(&format!("resume: {}", s.resume)),
            sanitize_line(&format!("started: {} ended: {}", s.started_at, s.ended_at)),
        ];
        lines.push(sanitize_line(&format!(
            "substance: {}",
            fmt_opt(&s.substance)
        )));
        lines.push(sanitize_line(&format!("active: {}s", s.active_secs)));
        if let Some(slug) = &s.project_slug {
            lines.push(sanitize_line(&format!("project: {slug}")));
        }
        if let Some(root) = &s.root_session_id {
            lines.push(sanitize_line(&format!("root: {root}")));
        }
        if let Some(parent) = &s.parent_session_id {
            lines.push(sanitize_line(&format!("parent: {parent}")));
        }
        lines.push(sanitize_line(&format!("children: {}", s.child_total)));
        if s.segment_chain.len() >= 2 {
            lines.push(sanitize_line(&format!(
                "segments: {}",
                s.segment_chain.join(" -> ")
            )));
        }
        sections.push(("summary".into(), lines));
    }

    let o = &card.outcome;
    if o.available {
        let mut lines = vec![sanitize_line(&format!("outcome: {}", o.verdict_word()))];
        if let Some(t) = &o.last_assistant_text {
            lines.push(sanitize_line(t));
        }
        for c in &o.checks {
            lines.push(sanitize_line(&format!("check: {}", c.text)));
        }
        if let Some(nc) = &o.not_committed {
            if nc.files.is_empty() {
                let cap = nc.caption.as_deref().unwrap_or("complete coverage");
                lines.push(sanitize_line(&format!("not committed: none ({cap})")));
            } else {
                for f in &nc.files {
                    lines.push(sanitize_line(&format!("not committed: {f}")));
                }
                if let Some(cap) = &nc.caption {
                    lines.push(sanitize_line(cap));
                }
            }
        }
        sections.push(("outcome".into(), lines));
    }

    let f = &card.files;
    if f.available {
        let mut lines = vec![sanitize_line(&format!(
            "files: {} total (read {}, edited {}, wrote {})",
            f.total, f.read, f.edited, f.wrote
        ))];
        for item in f.files.iter().take(10) {
            lines.push(sanitize_line(&format!("  {} ({})", item.path, item.action)));
        }
        if f.files.len() > 10 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                f.files.len() - 10
            )));
        }
        sections.push(("files".into(), lines));
    }

    let d = &card.decisions;
    if d.available {
        let mut lines = vec![sanitize_line(&format!("decisions: {}", d.total))];
        for item in d.decisions.iter().take(5) {
            lines.push(sanitize_line(&format!("  {}: {}", item.kind, item.prompt)));
        }
        if d.decisions.len() > 5 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                d.decisions.len() - 5
            )));
        }
        sections.push(("decisions".into(), lines));
    }

    let c = &card.commits;
    if c.available {
        let mut lines = vec![sanitize_line(&format!("commits: {}", c.total))];
        for item in c.commits.iter().take(5) {
            lines.push(sanitize_line(&format!(
                "  {} {}",
                item.kind,
                item.subject.as_deref().unwrap_or("—")
            )));
        }
        if c.commits.len() > 5 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                c.commits.len() - 5
            )));
        }
        sections.push(("commits".into(), lines));
    }

    let r = &card.research;
    if r.available {
        let mut line = format!("research: {} total", r.total);
        if !r.kind_counts.is_empty() {
            let counts: Vec<String> = r
                .kind_counts
                .iter()
                .map(|(k, n)| format!("{k} {n}"))
                .collect();
            line.push_str(&format!(" ({})", counts.join(", ")));
        }
        let mut lines = vec![sanitize_line(&line)];
        for item in r.items.iter().take(5) {
            lines.push(sanitize_line(&format!("  {}: {}", item.kind, item.query)));
        }
        if r.items.len() > 5 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                r.items.len() - 5
            )));
        }
        sections.push(("research".into(), lines));
    }

    let c = &card.comments;
    if c.available {
        let mut lines = vec![sanitize_line(&format!(
            "comments: open {}, raised {}",
            c.open_count, c.raised_count
        ))];
        for item in c.open.iter().take(5) {
            lines.push(sanitize_line(&format!("  open: {}", item.body)));
        }
        if c.open.len() > 5 {
            lines.push(sanitize_line(&format!("  … and {} more", c.open.len() - 5)));
        }
        for item in c.raised.iter().take(5) {
            lines.push(sanitize_line(&format!("  raised: {}", item.body)));
        }
        if c.raised.len() > 5 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                c.raised.len() - 5
            )));
        }
        sections.push(("comments".into(), lines));
    }

    let r = &card.readings;
    if r.available {
        let mut lines = vec![sanitize_line(&format!("readings: {}", r.total))];
        for item in r.items.iter().take(5) {
            lines.push(sanitize_line(&format!(
                "  {} ({})",
                item.title.as_deref().unwrap_or(&item.artifact_id),
                item.opened_at
            )));
        }
        if r.items.len() > 5 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                r.items.len() - 5
            )));
        }
        sections.push(("readings".into(), lines));
    }

    let m = &card.memory;
    if m.available {
        let mut lines = vec![sanitize_line(&format!(
            "memory: {} produced, {} recalled",
            m.produced_total,
            m.recalled.len()
        ))];
        for id in m.produced.iter().take(5) {
            lines.push(sanitize_line(&format!("  {id}")));
        }
        if m.produced.len() > 5 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                m.produced.len() - 5
            )));
        }
        sections.push(("memory".into(), lines));
    }

    let l = &card.live;
    if l.available {
        let lines = vec![sanitize_line(&format!(
            "live: {:?}, {:?}, {:?}, {}s ago{}",
            l.state,
            l.holder,
            l.confidence,
            l.since_secs.unwrap_or(0),
            if l.blocked.unwrap_or(false) {
                " (blocked)"
            } else {
                ""
            }
        ))];
        sections.push(("live".into(), lines));
    }

    let s = &card.slate;
    if s.available {
        let mut lines = vec![sanitize_line(&format!(
            "slate {}: {} matching, head {}",
            s.slug.as_deref().unwrap_or("—"),
            s.total,
            s.head_seq.unwrap_or(0)
        ))];
        for p in s.posts.iter().take(5) {
            lines.push(sanitize_line(&format!(
                "  #{} {}: {}",
                p.seq, p.kind, p.line
            )));
        }
        if s.posts.len() > 5 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                s.posts.len() - 5
            )));
        }
        sections.push(("slate".into(), lines));
    }

    let c = &card.children;
    if c.available {
        let mut lines = vec![sanitize_line(&format!("children: {}", c.total))];
        for item in c.items.iter().take(5) {
            lines.push(sanitize_line(&format!(
                "  {} {}",
                item.session_id, item.display_name
            )));
        }
        if c.items.len() > 5 {
            lines.push(sanitize_line(&format!(
                "  … and {} more",
                c.items.len() - 5
            )));
        }
        sections.push(("children".into(), lines));
    }

    sections
}

impl OutcomeLane {
    /// Its one-word verdict ("verified | failed | not_extracted | …")
    /// for the text renderer (the lane's structured `verdict` stands).
    fn verdict_word(&self) -> String {
        match self.verdict {
            crate::sessions::checks::Verdict::Verified => "verified".into(),
            crate::sessions::checks::Verdict::Failing => "failing".into(),
            crate::sessions::checks::Verdict::Unconfirmed => "unconfirmed".into(),
            crate::sessions::checks::Verdict::Stale => "stale".into(),
            crate::sessions::checks::Verdict::Unchecked => "unchecked".into(),
            crate::sessions::checks::Verdict::NoEdits => "no_edits".into(),
            crate::sessions::checks::Verdict::NotExtracted => "not_extracted".into(),
        }
    }
}

/// The daemon-rendered plain text of a card: pure, deterministic, every
/// line <= 80 chars, sections in lane order, one line per unavailable lane
/// with its reason. The CLI prints this verbatim and never formats lanes
/// of its own.
pub fn render_text(card: &SessionCard) -> String {
    let mut out = String::new();
    let sections = render_section_lines(card);

    let mut push = |line: String| {
        out.push_str(&sanitize_line(&line));
        out.push('\n');
    };

    // Lane order is the struct order: summary, outcome, files, decisions,
    // commits, research, comments, readings, memory, live, slate,
    // children, terminal.
    for (name, _) in &sections {
        let _ = name;
    }
    // Unavailable lanes interleaved in lane order: rebuild the status of
    // each lane beside its section body.
    let lanes: Vec<(&str, bool, &Option<String>, Option<Vec<String>>)> = vec![
        (
            "summary",
            card.summary.available,
            &card.summary.reason,
            section_of(&sections, "summary"),
        ),
        (
            "outcome",
            card.outcome.available,
            &card.outcome.reason,
            section_of(&sections, "outcome"),
        ),
        (
            "files",
            card.files.available,
            &card.files.reason,
            section_of(&sections, "files"),
        ),
        (
            "decisions",
            card.decisions.available,
            &card.decisions.reason,
            section_of(&sections, "decisions"),
        ),
        (
            "commits",
            card.commits.available,
            &card.commits.reason,
            section_of(&sections, "commits"),
        ),
        (
            "research",
            card.research.available,
            &card.research.reason,
            section_of(&sections, "research"),
        ),
        (
            "comments",
            card.comments.available,
            &card.comments.reason,
            section_of(&sections, "comments"),
        ),
        (
            "readings",
            card.readings.available,
            &card.readings.reason,
            section_of(&sections, "readings"),
        ),
        (
            "memory",
            card.memory.available,
            &card.memory.reason,
            section_of(&sections, "memory"),
        ),
        (
            "live",
            card.live.available,
            &card.live.reason,
            section_of(&sections, "live"),
        ),
        (
            "slate",
            card.slate.available,
            &card.slate.reason,
            section_of(&sections, "slate"),
        ),
        (
            "children",
            card.children.available,
            &card.children.reason,
            section_of(&sections, "children"),
        ),
    ];
    for (name, available, reason, body) in lanes {
        if available {
            if let Some(lines) = body {
                for line in lines {
                    push(line);
                }
            }
        } else {
            push(format!(
                "{name}: unavailable ({})",
                reason.as_deref().unwrap_or("unreadable")
            ));
        }
    }
    push("terminal: unavailable (no terminal runner)".to_string());
    out
}

fn section_of(sections: &[(String, Vec<String>)], name: &str) -> Option<Vec<String>> {
    sections
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, l)| l.clone())
}

// ---------------------------------------------------------------------------
// Tests — every behaviour here is pinned by a test that fails without it.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_card() -> SessionCard {
        SessionCard {
            schema: SESSION_CARD_SCHEMA.to_string(),
            session_id: "sess-1".to_string(),
            kb: "smoke".to_string(),
            artifact_id: "a000000000001".to_string(),
            text: String::new(),
            summary: SummaryLane::default(),
            outcome: OutcomeLane::unavailable("x"),
            files: FilesLane::default(),
            decisions: DecisionsLane::default(),
            commits: CommitsLane::default(),
            research: ResearchLane::default(),
            comments: CommentsLane::default(),
            readings: ReadingsLane::default(),
            memory: MemoryLane::default(),
            live: LiveLane::unavailable("not live"),
            slate: SlateLane::unavailable("no slate"),
            children: ChildrenLane::default(),
            terminal: TerminalLane::constant(),
        }
    }

    fn available_summary() -> SummaryLane {
        SummaryLane {
            available: true,
            reason: None,
            title: Some("A session".to_string()),
            display_name: "A session".to_string(),
            harness: "omp".to_string(),
            project_slug: Some("demo".to_string()),
            substance: Some("routine".to_string()),
            active_secs: 42,
            started_at: 100,
            ended_at: 200,
            segment_chain: Vec::new(),
            root_session_id: Some("root-1".to_string()),
            parent_session_id: None,
            child_total: 3,
            resume: "omp -r root-1".to_string(),
        }
    }

    #[test]
    fn card_lanes_cover_every_declared_lane_and_terminal_is_constant_unavailable() {
        let mut card = empty_card();
        card.summary = available_summary();
        card.outcome = OutcomeLane {
            available: true,
            reason: None,
            last_assistant_text: Some("Done.".to_string()),
            verdict: crate::sessions::checks::Verdict::Verified,
            checks: Vec::new(),
            not_committed: Some(NotCommitted {
                files: Vec::new(),
                commits_considered: 2,
                commits_total: 2,
                capped: false,
                unresolved_commits: 0,
                caption: None,
            }),
        };
        card.files = FilesLane {
            available: true,
            ..Default::default()
        };
        card.decisions = DecisionsLane {
            available: true,
            ..Default::default()
        };
        card.commits = CommitsLane {
            available: true,
            ..Default::default()
        };
        card.research = ResearchLane {
            available: true,
            ..Default::default()
        };
        card.comments = CommentsLane {
            available: true,
            ..Default::default()
        };
        card.readings = ReadingsLane {
            available: true,
            ..Default::default()
        };
        card.memory = MemoryLane {
            available: true,
            ..Default::default()
        };
        card.live = LiveLane::unavailable("not live");
        card.slate = SlateLane {
            available: true,
            ..Default::default()
        };
        card.children = ChildrenLane {
            available: true,
            ..Default::default()
        };
        let v = serde_json::to_value(&card).unwrap();
        for key in [
            "schema",
            "session_id",
            "kb",
            "artifact_id",
            "text",
            "summary",
            "outcome",
            "files",
            "decisions",
            "commits",
            "research",
            "comments",
            "readings",
            "memory",
            "live",
            "slate",
            "children",
            "terminal",
        ] {
            assert!(v.get(key).is_some(), "missing lane key {key}");
        }
        assert_eq!(v["schema"], "kb-session-card/1");
        // Every lane carries an explicit `available` flag.
        for key in [
            "summary",
            "outcome",
            "files",
            "decisions",
            "commits",
            "research",
            "comments",
            "readings",
            "memory",
            "live",
            "slate",
            "children",
            "terminal",
        ] {
            assert!(
                v[key].get("available").is_some(),
                "lane {key} has no explicit available flag"
            );
        }
        // The terminal lane is constant: false + the fixed reason.
        assert_eq!(v["terminal"]["available"], false);
        assert_eq!(v["terminal"]["reason"], "no terminal runner");
        assert_eq!(TerminalLane::constant(), TerminalLane::constant());
        // An unavailable lane carries its reason and default payloads.
        assert_eq!(v["live"]["available"], false);
        assert_eq!(v["live"]["reason"], "not live");
        // The rendered text always carries the terminal line.
        let text = render_text(&card);
        assert!(
            text.contains("terminal: unavailable (no terminal runner)"),
            "term lane line missing: {text:?}"
        );
    }

    #[test]
    fn render_text_is_pure_and_lists_unavailable_lanes() {
        let mut card = empty_card();
        card.summary = available_summary();
        // An injected escape in a transcript-derived value must never reach
        // the operator's terminal.
        card.summary.display_name = "Evil \u{1b}[31mred\u{1b}[0m".to_string();
        let text = render_text(&card);
        // No control characters (incl. \r and \x1b) anywhere on the card.
        for line in text.lines() {
            assert!(
                !line.chars().any(char::is_control),
                "control char in {line:?}"
            );
            assert!(line.chars().count() <= CARD_TEXT_LINE_MAX_CHARS, "{line:?}");
        }
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains('\r'));
        // Unavailable lanes are listed with their reason, one line each.
        assert!(text.contains("live: unavailable (not live)"), "{text:?}");
        assert!(text.contains("slate: unavailable (no slate)"), "{text:?}");
        // Pure: same input -> identical output.
        assert_eq!(render_text(&card), text);
    }

    #[test]
    fn render_text_truncates_long_values_to_the_line_budget() {
        let mut card = empty_card();
        card.summary = available_summary();
        card.summary.display_name = "x".repeat(500);
        let text = render_text(&card);
        for line in text.lines() {
            assert!(line.chars().count() <= CARD_TEXT_LINE_MAX_CHARS, "{line:?}");
        }
        assert!(text.contains('…'), "truncation must end with an ellipsis");
    }

    #[test]
    fn not_committed_is_capped_and_captioned() {
        // 25 resolved commits capped to 20 -> caption even when the diff is empty.
        let diff = not_committed_diff(&[], &[]);
        assert!(diff.is_empty());
        let cap = not_committed_caption(25, 20, 0).expect("a capped check is captioned");
        assert_eq!(
            cap,
            "checked the newest 20 of 25 commits; files may be committed earlier"
        );
        // Unresolved commits alone are also partial coverage.
        assert!(not_committed_caption(4, 4, 2).is_some());
        // Full coverage stays silent.
        assert_eq!(not_committed_caption(4, 4, 0), None);
        // The diff is sorted + deduped, untouched paths survive.
        let edited = vec!["/r/a".to_string(), "/r/b".to_string(), "/r/a".to_string()];
        let touched = vec!["/r/a".to_string()];
        assert_eq!(
            not_committed_diff(&edited, &touched),
            vec!["/r/b".to_string()]
        );
        // The cap constant is the one the server loop uses.
        assert_eq!(NOT_COMMITTED_COMMIT_CAP, 20);
    }
}
