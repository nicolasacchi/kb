//! v0.14 S2 — session enrichment. A "session" in kb is a memory
//! artifact tagged `kb-category=memory-session` (typically the Stop
//! hook's verbatim Claude Code transcript wrapper). The artifact
//! itself lives in lance like any other; this module turns its raw
//! `<pre>` JSONL into a small, indexable enrichment row (session id,
//! start time, message count, first user prompt preview) the
//! `/api/sessions` route serves without re-scraping the (potentially
//! multi-MB) HTML body on every request.
//!
//! The module is pure — `parse_session_html` takes the artifact's
//! HTML body, its source filename, and the file mtime, and returns
//! deterministic `SessionFacts`. The storage actor calls it from
//! the indexer after the lance upsert; results land in the V0008
//! `sessions` sqlite table.

/// The `session-replay/1` timeline extractor (W3 R-a) — a PURE, clock-free
/// pass over the recovered `<pre>` JSONL that recovers the per-event
/// timestamps sqlite never stored. Nothing in this module calls it; it is the
/// shared foundation the replay CLI verb and the SPA reader draw from.
pub mod replay;

/// The `session-view/1` interpretation engine (sessions-rethink W1) — join
/// pass, interpretation catalog, closure/outcome, and the incremental
/// `ViewCarry`/`view_append` constructor. `session_render` migrates onto this
/// module this same wave; the CLI presenter (`kb sessions read`) and the
/// `/api/sessions/{sid}/view` wire route are later waves over the same IR.
pub mod view;

/// The live-transcript tailer + shared resolver (sessions-rethink W7,
/// R15/LF-4): a byte-offset `TailReader` with complete-line discipline, the
/// bootstrap-window reader `view::view_bootstrap` consumes, and
/// `resolve_live_transcript` — the ONE sid→path resolution the `/live`
/// route AND `kb sessions read --live`/`--follow` both call.
pub mod tail;

/// The ratified sessions constants (sessions-rethink W0): the outcome anchor
/// id, the closed harness set + default, the active-time clamp, the wire caps
/// and the presence windows. Re-exported below, so every consumer says
/// `kb_core::sessions::HARNESS_DEFAULT`, never a literal.
pub mod constants;

/// LSC-1 (`docs/research/kb-live-sessions-cockpit-2026-08.html`) — the
/// two-axis live-state model (who holds the ball × how long since it
/// moved) plus the Claude Code transcript adapter, `classify_claude_
/// transcript`, and its bounded `scan_claude_projects` discovery walk.
/// Pull-only / direct-disk in this phase (`kb sessions status --local`);
/// nothing here talks to the daemon.
pub mod live;

pub use constants::{
    ABANDON_AFTER_SECS, ACTIVE_DELTA_CLAMP_SECS, ACTIVE_WINDOW_SECS, COLD_AFTER_SECS, HARNESSES,
    HARNESS_DEFAULT, LAST_ASSISTANT_TEXT_MAX_CHARS, LIVE_TAIL_BYTES, LIVE_WINDOW_SECS,
    OUTCOME_WIRE_MAX_CHARS, SES_OUTCOME_ANCHOR, STALL_AFTER_SECS,
};

/// LSC-5 (`docs/research/kb-live-sessions-cockpit-2026-08.html` §4) — the
/// four non-Claude harness adapters (codex/grok/kimi/opencode), each a
/// sibling of [`live`]'s Claude Code adapter sharing its shape, plus
/// [`live_adapters::scan_all`] — the ONE entry point `kb sessions status`
/// fans out to across every harness. Pull-only / direct-disk, same as
/// [`live`]; nothing here talks to the daemon either.
pub mod live_adapters;

/// W3.A — the `[projects.*]` config registry: read-time relabel/merge layer
/// over `derive_project`'s pure ladder. See the module doc for the full
/// derivation-vs-registry split.
pub mod projects;

/// CT-E5 — the PURE half of "save a session thread as a narrative reading
/// list": lane ordering, de-duplication, entry notes, and the list
/// description (incl. the inert-`code_url`-derived kb-code session-diff
/// link). The route owns the storage reads; this module owns the story.
pub mod narrative;

pub use projects::{
    project_registry, registry_entry, resolve_project_filter, resolve_registry_project,
    set_project_registry, ProjectDef, ProjectFilter, ResolvedProject,
};

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};

/// Cap on the first-prompt preview surfaced in `/api/sessions` rows
/// and the SPA session row preview. Long enough to recognise a
/// session at a glance; short enough that even pathological
/// JSON-stringified content blocks don't blow out the list response.
const PROMPT_PREVIEW_MAX_CHARS: usize = 200;

/// The `kb-category` value the Stop-hook capture (`kb-capture.sh`) stamps on
/// every session-transcript artifact. The load-bearing string the parser, the
/// enrich hook (`enrich::is_memory_session_category`), and the search/recall
/// exclusion all key on: a `memory-session` row is an internal *episodic* log,
/// kept out of default document search and the every-turn recall push (R0),
/// reachable only via `/api/sessions` and `kb recollect`.
pub const MEMORY_SESSION_CATEGORY: &str = "memory-session";

/// Parsed enrichment for one memory-session artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionFacts {
    /// Claude Code session id. Prefer the `<meta name="kb-session">`
    /// content; fall back to the source filename
    /// `session-<ts>-<sid>.html`. Last resort: the filename stem.
    pub session_id: String,
    /// Unix seconds when the session began. From the filename
    /// timestamp if parseable, else the file mtime.
    pub started_at: i64,
    /// Number of JSONL records in the embedded transcript `<pre>`.
    /// Zero when the artifact has no `<pre>` block or it's empty.
    pub message_count: u32,
    /// First user-authored prompt, truncated to ~200 chars. None when
    /// the transcript has no eligible user message (e.g., the file is
    /// not a Claude Code transcript at all but some other
    /// memory-session artifact).
    pub first_user_prompt: Option<String>,
}

/// What a session did to a file. Derived from the tool-call `name` in the
/// transcript: `Read` → [`Read`](FileAction::Read), `Write` →
/// [`Write`](FileAction::Write), `Edit`/`MultiEdit`/`NotebookEdit` →
/// [`Edit`](FileAction::Edit).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileAction {
    Read,
    Write,
    Edit,
}

impl FileAction {
    /// Lowercase wire token (`"read" | "write" | "edit"`), the value
    /// stored in `session_files.action` and round-tripped over HTTP.
    pub fn as_str(self) -> &'static str {
        match self {
            FileAction::Read => "read",
            FileAction::Write => "write",
            FileAction::Edit => "edit",
        }
    }

    /// Parse the wire token back. `None` on an unknown value (forward-compat
    /// with a future action the SPA/CLI hasn't taught this enum).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "read" => Some(FileAction::Read),
            "write" => Some(FileAction::Write),
            "edit" => Some(FileAction::Edit),
            _ => None,
        }
    }
}

/// One file a session touched, as parsed from a tool-call. `abs_path` is
/// verbatim from the transcript (it may be absolute OR source-relative —
/// Claude Code records whatever the agent passed); corpus resolution +
/// canonicalisation happens later, in the enrich hook (S3), not here.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileTouch {
    pub path: String,
    pub action: FileAction,
}

/// A steering moment in a session — the spine of the "follow-the-work"
/// decisions log (S9). Either an AskUserQuestion answer (`question`) or an
/// ExitPlanMode approval (`plan`). LLM-free, extracted deterministically from
/// the structured `toolUseResult.answers` (preferred) or the prose fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// `"question"` | `"plan"`.
    pub kind: String,
    /// The question text (for `question`) or a short label (for `plan`).
    pub prompt: String,
    /// The chosen answer; `None` for a bare plan approval.
    pub answer: Option<String>,
}

/// A VCS action a session took (P5) — `git commit` / `push` / `tag`, detected
/// from a Bash tool-call and its result. The SHA is best-effort (parsed from
/// the command output); framed as detected, never ground truth (invariant #10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    /// `"commit"` | `"push"` | `"tag"`.
    pub kind: String,
    /// Short SHA parsed from the command output, when present.
    pub sha: Option<String>,
    /// The commit subject (`-m`/`-F` message) or a short label. `None` when
    /// a message flag is present but unrecoverable from the transcript
    /// (plain `-F <file>`: the message text lives in a file kb never sees)
    /// — flagged for capture-time resolution later, never fabricated.
    pub subject: Option<String>,
}

/// R4 — one research / tool-usage signal a session produced: a kb-CLI search,
/// a web search/fetch, a subagent, a skill/MCP tool, or a plan presentation.
/// Heuristic-parsed from the transcript's `tool_use` blocks and framed as
/// "detected, not ground truth" (invariant #10) — the shapes vary by Claude
/// Code version, so extraction is version-tolerant + golden-test-pinned.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Research {
    /// `"kb_search" | "web" | "skill" | "subagent" | "plan_span" | "artifact_open"`.
    pub kind: String,
    /// The query / target / label (best-effort; may be empty).
    pub query: String,
}

/// The rich, transcript-derived picture of one session. Pure output of
/// [`parse_session_activity`] over the unescaped JSONL — the single source
/// the index enrichment (V0017 `sessions` row + `session_files`) and the
/// render layer both draw from, so the persisted row and the rendered view
/// can never drift (the old double `first_user_prompt` impls did).
///
/// Everything here is local to the transcript: no corpus membership, no
/// artifact-id resolution, no clock — deterministic given the bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionActivity {
    /// The authoritative Claude Code session id, read from the transcript's own
    /// `sessionId` field (first non-empty wins). Ground truth — preferred over
    /// the `<meta name="kb-session">`, which a buggy capture hook truncated to
    /// 24 chars. `None` for transcripts that predate the field. Powers the
    /// `claude -r <id>` resume command + the session↔memories link.
    pub session_id: Option<String>,
    /// The AI-generated title (`{"type":"ai-title","aiTitle":…}`), the LAST
    /// one in the transcript (Claude Code refines it as the session grows).
    pub ai_title: Option<String>,
    /// The modal working directory — the `cwd` that appears on the most
    /// records. `None` when no record carries a `cwd`.
    pub cwd: Option<String>,
    /// Every distinct `cwd` the session visited, in first-seen order. A
    /// session that spans dirs (≈60% do) keeps the full set for the UI.
    pub all_cwds: Vec<String>,
    /// The first non-empty `gitBranch` seen.
    pub git_branch: Option<String>,
    /// The first genuinely user-typed prompt (see the two-pass rule in
    /// [`parse_session_activity`]), truncated to [`PROMPT_PREVIEW_MAX_CHARS`].
    pub first_user_prompt: Option<String>,
    /// The max event `timestamp` (unix secs) — the real end of the session,
    /// unlike the capture-mtime `ended_at` the V0008 row used to store.
    pub ended_at: Option<i64>,
    /// Count of non-empty JSONL records (back-compat with `message_count`).
    pub message_count: u32,
    /// Files touched by tool-calls, deduped on `(path, action)`, in
    /// first-seen order.
    pub files: Vec<FileTouch>,
    /// Authoritative edited-file set: the keys of the LAST
    /// `file-history-snapshot.trackedFileBackups` (Claude Code's own record
    /// of which files it backed up because it edited them).
    pub edited_paths: Vec<String>,
    /// S9 — steering moments (AskUserQuestion answers + plan approvals), in
    /// transcript order. The decisions log.
    pub decisions: Vec<Decision>,
    /// S9 — total tokens (input + output) summed over assistant turns.
    pub token_total: u64,
    /// S9 — number of tool calls (tool_use blocks).
    pub tool_calls: u32,
    /// S9 — the model the session ran on (last assistant `model`).
    pub model: Option<String>,
    /// S9 — count of error tool-results (is_error), a coarse "did things go
    /// wrong" signal. Framed as detected, never ground truth.
    pub error_count: u32,
    /// P5 — VCS actions the session produced (git commit/push/tag), detected
    /// from Bash tool-calls + their output. "What shipped from this work."
    pub commits: Vec<Commit>,
    /// R4 — research / tool-usage signals (kb & web searches, subagents,
    /// skills/MCP, plan presentations), deduped on `(kind, query)`, in
    /// first-seen order. "How kb was used / what was explored."
    pub research: Vec<Research>,
    /// W0.2 — count of Agent/Task delegations whose parent-transcript
    /// `toolUseResult` came back with real stats (a SYNCHRONOUS completion).
    /// ASYNC/BACKGROUND delegations (the harness default) do NOT count here
    /// — see `subagent_launched_unstatted`.
    pub subagent_count: u32,
    /// W0.2 — summed `totalTokens` over every completed-with-stats subagent.
    pub subagent_tokens: u64,
    /// W0.2 — summed `totalToolUseCount` over every completed-with-stats
    /// subagent.
    pub subagent_tool_calls: u32,
    /// W0.2 — summed `toolStats.editFileCount` over every completed-with-
    /// stats subagent. This is an edit-OPERATION count (the real transcript
    /// shape carries no distinct-file list for a subagent), same caveat as
    /// the parent session's own tool-call counters — detected, not ground
    /// truth (#10).
    pub subagent_files_edited: u32,
    /// W0.2 — count of Agent/Task delegations that produced NO stats: an
    /// async stub (`status:"async_launched"`) or any other agentId-bearing
    /// result missing `totalTokens`. Kept separate from the stat columns so
    /// a session that launched agents but got no numbers back never reads
    /// identically to a session that launched none — their real numbers live
    /// only in the per-agent sidecar files (W0.4/W0.5).
    pub subagent_launched_unstatted: u32,
    /// R5/V0029 — the harness that produced this transcript, from the FIRST
    /// `type:"adapter-meta"` record's `harness` field (rung 1 of the R5
    /// ladder; the `<meta name="kb-harness">` fallback and the `claude`
    /// default live at the enrich-hook layer, which has the raw HTML head —
    /// this parser only sees the `<pre>` JSONL). `None` for a bare Claude
    /// Code transcript (no adapter-meta line at all). Mirrors
    /// `sessions::view::ViewCarry`'s identical rung-1 check verbatim — two
    /// separate passes over the same bytes, deliberately not unified (they
    /// serve different engines; see the module docs).
    pub harness: Option<String>,
    /// V0029 — first non-empty top-level `version` field across the JSONL
    /// (the `first_transcript_field(jsonl, "version")` rule, computed inline
    /// here instead of via a second pass over the transcript).
    pub cc_version: Option<String>,
    /// R3/V0029 — the session's CLOSURE: the last real (non-wrapper,
    /// non-synthetic, main-thread) assistant prose in the transcript, same
    /// eligibility rule as the standalone [`closing_assistant_text`] (which
    /// `session-view/1`'s outcome now derives from the walk's Assistant Prose
    /// items via the shared substantial/any last-wins rule) — folded into
    /// this single-pass parser too so the persisted `sessions.last_assistant_text`
    /// column and the digest excerpt don't pay a second full-transcript scan.
    /// `None` for a prose-less husk capture.
    pub last_assistant_text: Option<String>,
    /// V0029 — count of `role:"user"` records that classify as
    /// [`UserTextClass::Real`] (typed, or clearing the wrapper-skip
    /// fallback) and are not sidechain — the honest "real prompts" number
    /// (P10: depth.md measured 1,341 raw `user` cards against ~54 real
    /// prompts in one session).
    pub user_turns: u32,
    /// R6/D4/V0029 — the honest ACTIVE duration in seconds: the per-delta
    /// clamped sum over every timestamped event, via the shared
    /// [`active_secs`] fn (the SAME fn `session-view/1`'s header uses), so
    /// the persisted column and the reader header can never disagree.
    pub active_secs: i64,
    /// Count of non-empty JSONL lines that failed `serde_json` parse and
    /// were skipped (skip-not-fail). Observability only — never a hard
    /// error, never a score term. Zero on a clean transcript.
    pub skipped_lines: u32,
}

impl SessionActivity {
    /// Number of files the session READ (an Edit/Write to a file also
    /// counts it as read upstream, but here we count read-action touches).
    pub fn files_read_count(&self) -> u32 {
        self.files
            .iter()
            .filter(|f| f.action == FileAction::Read)
            .count() as u32
    }

    /// Number of distinct files the session EDITED or WROTE. Prefers the
    /// authoritative `edited_paths` (Claude Code's own backup set); falls
    /// back to counting edit/write tool-calls when no snapshot was recorded.
    pub fn files_edited_count(&self) -> u32 {
        if !self.edited_paths.is_empty() {
            return self.edited_paths.len() as u32;
        }
        let mut seen = std::collections::BTreeSet::new();
        for f in &self.files {
            if matches!(f.action, FileAction::Edit | FileAction::Write) {
                seen.insert(f.path.as_str());
            }
        }
        seen.len() as u32
    }
}

/// The full parse of a memory-session artifact: the slim [`SessionFacts`]
/// (id/start/count/first-prompt, what the V0008 row stored) plus the rich
/// [`SessionActivity`]. One pass over the embedded `<pre>` — callers that
/// only need facts use [`parse_session_html`]; the enrich hook uses this.
#[derive(Debug, Clone)]
pub struct SessionParse {
    pub facts: SessionFacts,
    pub activity: SessionActivity,
}

/// Parse a memory-session HTML artifact into its enrichment row.
///
/// `html` is the raw artifact body as written to disk (the
/// kb-capture.sh wrapper is `<!DOCTYPE html>…<pre>{escaped JSONL}</pre>…`).
/// `filename` is the source-relative filename (used to back-fill the
/// session id for pre-v0.14 artifacts that don't carry the meta).
/// `mtime_unix` backs `started_at` when the filename timestamp is not
/// parseable.
pub fn parse_session_html(html: &str, filename: &str, mtime_unix: i64) -> SessionFacts {
    parse_session_html_full(html, filename, mtime_unix).facts
}

/// Full parse: [`SessionFacts`] + the rich [`SessionActivity`], from one
/// pass over the transcript. The enrich hook (S3) uses this to populate the
/// V0017 `sessions` row (title/cwd/git_branch/counts) and `session_files`.
///
/// `started_at` comes from the filename (the transcript body doesn't carry the
/// kb-capture wrapper's timestamp). `session_id` prefers the transcript's own
/// `sessionId` (ground truth) over the `<meta name="kb-session">`, repairing the
/// capture hook's 24-char truncation; the rest is whatever
/// [`parse_session_activity`] extracts from the `<pre>`.
pub fn parse_session_html_full(html: &str, filename: &str, mtime_unix: i64) -> SessionParse {
    let started_at = filename_timestamp(filename).unwrap_or(mtime_unix);

    let activity = extract_pre(html)
        .map(|raw| parse_session_activity(&html_unescape(&raw)))
        .unwrap_or_default();

    // Prefer the transcript's own `sessionId` (the authoritative Claude id) over
    // the `<meta name="kb-session">` — a buggy capture hook truncated the meta to
    // 24 chars, which broke `claude -r` and the session↔memories link (the doc
    // `kb_session` marker kept the full id). Fall back to meta → filename → stem
    // for transcripts that predate the field.
    let session_id = activity
        .session_id
        .clone()
        .or_else(|| meta_session(html))
        .or_else(|| filename_session_id(filename))
        .unwrap_or_else(|| filename_stem(filename));

    let facts = SessionFacts {
        session_id,
        started_at,
        message_count: activity.message_count,
        first_user_prompt: activity.first_user_prompt.clone(),
    };
    SessionParse { facts, activity }
}

/// Cap on the digest's `body_text_excerpt` form — matches the parser's
/// `BODY_EXCERPT_MAX_CHARS` so a session's search-card snippet has the same
/// depth as a normal artifact's.
const DIGEST_EXCERPT_MAX_CHARS: usize = 400;

/// R1 — assemble a deterministic, high-signal **insight digest** for a
/// session: the text the *index* sees in place of the raw, multi-MB JSONL
/// `<pre>` body. Built purely from the already-parsed [`SessionParse`] —
/// title, the human's opening ask, steering decisions (the "why" spine),
/// commit subjects, touched-file basenames, and project/branch context — so
/// embedding + BM25 + the cross-encoder reranker retrieve on *meaning*
/// rather than transcript scaffolding (`"role"`, `"tool_use"`, JSON keys).
///
/// The raw transcript on disk is untouched (invariant #27); only what the
/// index matches against changes. Deterministic given the bytes (no clock,
/// no I/O) — golden-tested. R4 extends this with extracted research queries.
pub fn session_digest(parse: &SessionParse) -> String {
    let a = &parse.activity;
    let mut lines: Vec<String> = Vec::new();
    let nonempty = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };

    // Title + the human's opening ask — the strongest "what was this about".
    if let Some(t) = a.ai_title.as_deref().and_then(nonempty) {
        lines.push(t);
    }
    if let Some(p) = parse.facts.first_user_prompt.as_deref().and_then(nonempty) {
        lines.push(p);
    }

    // Steering decisions (AskUserQuestion answers + plan approvals) — the
    // "why was it done this way" spine.
    for d in &a.decisions {
        let Some(prompt) = nonempty(&d.prompt) else {
            continue;
        };
        match d.answer.as_deref().and_then(nonempty) {
            Some(ans) => lines.push(format!("decision: {prompt} -> {ans}")),
            None => lines.push(format!("decision: {prompt}")),
        }
    }

    // R4 — what was researched / explored (kb & web searches, subagents,
    // skills): the topic surface that makes `kb recollect` match on intent.
    // W4/R8/ADD-2 — `grok_job` rows are EXCLUDED here: the digest budget is
    // spent (invariant #11 amendment — exactly ONE reindex, already run) and
    // a bare job ulid is opaque, not topical signal a ranking pass should
    // weigh. The row still lives in `session_research` for the `by-job` join
    // and `kb sessions show --section research`; it just never reaches the
    // index.
    let research_qs: Vec<&str> = a
        .research
        .iter()
        .filter(|r| r.kind != "grok_job")
        .filter_map(|r| nonempty(&r.query).map(|_| r.query.trim()))
        .collect();
    if !research_qs.is_empty() {
        lines.push(format!("researched: {}", research_qs.join("; ")));
    }

    // What shipped — commit subjects (detected, not ground truth; #10).
    let commit_subjects: Vec<String> = a
        .commits
        .iter()
        .filter_map(|c| c.subject.as_deref().and_then(nonempty))
        .collect();
    if !commit_subjects.is_empty() {
        lines.push(format!("commits: {}", commit_subjects.join("; ")));
    }

    // Touched files — basenames only (full paths are noisy + leak structure).
    // Authoritative edited set first, then every read/edit/write touch.
    let mut basenames: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for p in a.edited_paths.iter().chain(a.files.iter().map(|f| &f.path)) {
        let base = p.rsplit(['/', '\\']).next().unwrap_or(p).trim();
        if !base.is_empty() && seen.insert(base.to_string()) {
            basenames.push(base.to_string());
        }
    }
    if !basenames.is_empty() {
        lines.push(format!("files: {}", basenames.join(" ")));
    }

    // R3/D1-A — the CLOSURE: what the session ended with. Ranks (R1: this is
    // `body`/embed/SQ5-chunk material, not evidence-only `code`), so
    // `kb recollect` can find a session by its outcome, not just its opening
    // ask. Capped shorter than the persisted `last_assistant_text` column —
    // this is one line among many in a digest, not the full closure.
    if let Some(closed) = a.last_assistant_text.as_deref().and_then(nonempty) {
        lines.push(format!(
            "closed: {}",
            truncate_chars_ellipsis(&closed, DIGEST_CLOSED_LINE_MAX_CHARS)
        ));
    }

    // Project context — the modal cwd's basename + the branch.
    let mut ctx = String::new();
    if let Some(cwd) = a.cwd.as_deref().and_then(nonempty) {
        let folder = cwd.rsplit(['/', '\\']).next().unwrap_or(&cwd);
        ctx.push_str("project: ");
        ctx.push_str(folder);
    }
    if let Some(b) = a.git_branch.as_deref().and_then(nonempty) {
        if !ctx.is_empty() {
            ctx.push(' ');
        }
        ctx.push_str("branch: ");
        ctx.push_str(&b);
    }
    if !ctx.is_empty() {
        lines.push(ctx);
    }

    lines.join("\n")
}

/// R3/D1-B — cap on the `closed:` line inside the full [`session_digest`]
/// body (a digest carries many lines; this is one of them). Shorter than
/// [`LAST_ASSISTANT_TEXT_MAX_CHARS`] (the persisted column) and
/// [`OUTCOME_WIRE_MAX_CHARS`] (the wire preview) — both of which show the
/// closure as the ONE headline fact rather than one line among several.
const DIGEST_CLOSED_LINE_MAX_CHARS: usize = 200;

/// R1/R3/D1-B — the `body_text_excerpt` form: the snippet shown on search and
/// recollect CARDS. Moonshots' operator decision D1-B (memo R3): recompose
/// the excerpt as **title · first-prompt · closed** rather than a raw prefix
/// of the full [`session_digest`] body — the digest's line order puts
/// decisions/research/files/project context BEFORE the closure, so a blind
/// 400-char prefix would usually never reach it. Composed directly from the
/// parse (not from the digest string) so it can never drift from what the
/// digest body actually contains, and the excerpt's own line order is
/// independent of the digest body's.
pub fn session_digest_excerpt(parse: &SessionParse) -> String {
    let a = &parse.activity;
    let nonempty = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = a.ai_title.as_deref().and_then(nonempty) {
        parts.push(t);
    }
    if let Some(p) = parse.facts.first_user_prompt.as_deref().and_then(nonempty) {
        parts.push(p);
    }
    if let Some(c) = a.last_assistant_text.as_deref().and_then(nonempty) {
        parts.push(format!("closed: {c}"));
    }
    truncate_chars(&parts.join(" · "), DIGEST_EXCERPT_MAX_CHARS)
}

// ---- R3 — `kb recollect` ranking + staleness ------------------------------

/// RRF-style rank constant for recollect, matching memory recall (`1/(60+r)`).
const RECOLLECT_RANK_K: f32 = 60.0;
/// Recency half-life (days) for recollect: a session this old contributes half
/// the recency weight. GENTLE on purpose — episodic "has this been done?"
/// should still surface year-old work, not bury it.
const RECOLLECT_RECENCY_HALFLIFE_DAYS: f32 = 365.0;
const SECONDS_PER_DAY_F32: f32 = 86_400.0;
/// A session older than this is flagged `stale` — its recorded rationale may
/// predate later rewrites of the same code. SURFACED to the agent, never used
/// to drop a hit (#27: episodes are immutable evidence, discounted not deleted).
pub const RECOLLECT_STALE_AFTER_DAYS: i64 = 180;

/// R3 — deterministic recollect relevance score: `rel × recency_decay`.
/// `rel = 1/(60 + rank)` (the per-corpus search rank, matching recall);
/// recency is a gentle exp half-life. Success signals (commits / errors) are
/// deliberately NOT in the score — they're surfaced separately and only break
/// near-ties, so a stronger digest match always wins (even a high-error
/// exploratory session outranks a tangentially-committing one). Deterministic
/// given `(rank, started_at, now_unix)`.
pub fn recollect_score(rank: usize, started_at: i64, now_unix: i64) -> f32 {
    let rel = 1.0 / (RECOLLECT_RANK_K + rank as f32);
    let age_days = ((now_unix - started_at).max(0) as f32) / SECONDS_PER_DAY_F32;
    let k = std::f32::consts::LN_2 / RECOLLECT_RECENCY_HALFLIFE_DAYS;
    rel * (-k * age_days).exp()
}

/// Whole-day age of a session at `now_unix` (clamped at 0).
pub fn session_age_days(started_at: i64, now_unix: i64) -> i64 {
    (now_unix - started_at).max(0) / 86_400
}

/// R3 — a recollect candidate before final ordering: the search rank plus the
/// cheap signals the ordering + surfaced fields read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecollectCandidate {
    pub session_id: String,
    pub rank: usize,
    pub started_at: i64,
    pub error_count: u32,
}

/// R3 — order recollect candidates deterministically: relevance×recency DESC,
/// then the cheap success tie-break (fewer errors), then recency, then id.
/// `now_unix` keeps recency deterministic for the caller and golden tests.
/// A stronger digest match (lower rank) wins even against a clean, committing
/// session, by construction (success is a tie-break, not a score term).
pub fn recollect_order(candidates: &mut [RecollectCandidate], now_unix: i64) {
    candidates.sort_by(|a, b| {
        let sa = recollect_score(a.rank, a.started_at, now_unix);
        let sb = recollect_score(b.rank, b.started_at, now_unix);
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.error_count.cmp(&b.error_count))
            .then_with(|| b.started_at.cmp(&a.started_at))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
}

/// Pure: extract the rich activity picture from the unescaped JSONL
/// transcript. Deterministic given the bytes — no clock, no I/O, no corpus
/// knowledge. This is the ONE parser both the index row and the render
/// layer draw from (it replaces the two divergent `first_user_prompt`
/// implementations that previously drifted).
///
/// First-prompt rule (two passes):
/// 1. **typed**: the first `role:"user"` record with `promptSource == "typed"`
///    — the gold signal on modern Claude Code transcripts (the only thing
///    the human actually typed; slash-command expansions carry a null
///    source).
/// 2. **fallback** (old transcripts predate `promptSource`): the first
///    `role:"user"` record that is not `isMeta`, not a sidechain, and whose
///    text doesn't start with a synthetic wrapper (`<command-…>`,
///    `<local-command-…>`, `<task-notification>`, `<system-reminder>`, …).
pub fn parse_session_activity(jsonl: &str) -> SessionActivity {
    parse_session_activity_full(jsonl).0
}

/// Full parse returning the activity plus the first non-empty top-level
/// `"agentId"` seen (sidecar ground truth). One walk — both
/// [`parse_session_activity`] and [`parse_subagent_jsonl`] share it so a
/// sidecar never pays a second full-transcript scan just for its id.
fn parse_session_activity_full(jsonl: &str) -> (SessionActivity, Option<String>) {
    let mut act = SessionActivity::default();
    let mut cwd_counts: Vec<(String, u32)> = Vec::new();
    // First-prompt candidates gathered in one pass; the typed winner is
    // chosen over the fallback after the scan so order can't matter.
    let mut typed_prompt: Option<String> = None;
    let mut fallback_prompt: Option<String> = None;
    let mut count: u32 = 0;
    let mut skipped_lines: u32 = 0;
    // Sidecar agent id — first non-empty top-level `"agentId"` wins (same
    // rule the old standalone `transcript_agent_id` scan used).
    let mut transcript_agent_id: Option<String> = None;
    // R6/D4/V0029 — every timestamped event, transcript order, fed to the
    // shared `active_secs` fn once at the end (never sorted — see that fn's
    // docs on out-of-order input).
    let mut event_times: Vec<i64> = Vec::new();
    // V0029/user_turns — count of `role:"user"` records classifying as
    // `UserTextClass::Real` and not sidechain.
    let mut user_turns: u32 = 0;
    // R3/V0029 — the same last-wins substantial/any pair `closing_assistant_text`
    // tracks, folded into this pass (see `SessionActivity::last_assistant_text`).
    let mut closing_substantial: Option<String> = None;
    let mut closing_any: Option<String> = None;
    // P5 — git Bash tool_use ids awaiting their result (id → the git actions
    // that command produced — a Vec because a chained `commit && push`
    // yields more than one), so a SHA in the tool_result can be paired back
    // to the command(s).
    let mut pending_git: std::collections::BTreeMap<String, Vec<(String, Option<String>)>> =
        std::collections::BTreeMap::new();
    // W4/R8/ADD-2 — grokclaude Bash tool_use ids awaiting their result (id →
    // the invoking command line, kept as the degraded fallback query when no
    // job ulid is confidently recoverable from the paired tool_result). See
    // `extract_grok_job`.
    let mut pending_grok: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    // Membership companions for insertion-ordered `act.files` / `act.research`
    // dedup — Vec stays the ordered store; HashSet is O(1) contains only.
    let mut files_seen: std::collections::HashSet<FileTouch> = std::collections::HashSet::new();
    let mut research_seen: std::collections::HashSet<Research> = std::collections::HashSet::new();

    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        count += 1;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            skipped_lines += 1;
            continue;
        };

        if transcript_agent_id.is_none() {
            if let Some(id) = v.get("agentId").and_then(|x| x.as_str()) {
                let t = id.trim();
                if !t.is_empty() {
                    transcript_agent_id = Some(t.to_string());
                }
            }
        }

        // sessionId — the authoritative Claude session id (first non-empty
        // wins; it's constant across a transcript). Ground truth for the
        // canonical session_id, repairing the capture hook's truncated meta.
        if act.session_id.is_none() {
            if let Some(sid) = v.get("sessionId").and_then(|x| x.as_str()) {
                if !sid.trim().is_empty() {
                    act.session_id = Some(sid.trim().to_string());
                }
            }
        }

        // ai-title (refined over the session — keep the LAST).
        if let Some(t) = v.get("aiTitle").and_then(|x| x.as_str()) {
            if !t.trim().is_empty() {
                act.ai_title = Some(t.trim().to_string());
            }
        }

        // R5/V0029 — harness ladder rung 1: the first `adapter-meta` record's
        // `harness` field. Mirrors `sessions::view::ViewCarry`'s identical
        // check verbatim (two engines, one rule).
        if act.harness.is_none() && v.get("type").and_then(|x| x.as_str()) == Some("adapter-meta") {
            if let Some(h) = v.get("harness").and_then(|x| x.as_str()) {
                if !h.trim().is_empty() {
                    act.harness = Some(h.trim().to_string());
                }
            }
        }

        // W5/R8/ADD-2 — the CHILD-side grok_job row: when THIS capture IS
        // itself a grok session (its own adapter-meta line, stamped by
        // `kb-capture-grok.sh`, carries `job_ulid`), record the SAME
        // `session_research` kind="grok_job" row the DRIVER side already
        // emits above (`extract_grok_job`, W4/R8/ADD-2) — one ulid, two
        // sides, no new table. `kb sessions by-job <ulid>` (the existing
        // route/verb) then returns BOTH the invoking (Driver) session and
        // this invoked (Child) session for the same ulid. Dedup mirrors
        // every other research push site.
        if v.get("type").and_then(|x| x.as_str()) == Some("adapter-meta") {
            if let Some(ulid) = v.get("job_ulid").and_then(|x| x.as_str()) {
                let ulid = ulid.trim();
                if !ulid.is_empty() {
                    let r = Research {
                        kind: "grok_job".to_string(),
                        query: ulid.to_string(),
                    };
                    if research_seen.insert(r.clone()) {
                        act.research.push(r);
                    }
                }
            }
        }

        // V0029 — first non-empty top-level `version` field (the
        // `first_transcript_field(jsonl, "version")` rule).
        if act.cc_version.is_none() {
            if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
                if !ver.trim().is_empty() {
                    act.cc_version = Some(ver.trim().to_string());
                }
            }
        }

        // cwd — tally for the modal key; remember first-seen order.
        if let Some(cwd) = v.get("cwd").and_then(|x| x.as_str()) {
            if !cwd.is_empty() {
                if let Some(slot) = cwd_counts.iter_mut().find(|(c, _)| c == cwd) {
                    slot.1 += 1;
                } else {
                    cwd_counts.push((cwd.to_string(), 1));
                    act.all_cwds.push(cwd.to_string());
                }
            }
        }

        // gitBranch — first non-empty wins.
        if act.git_branch.is_none() {
            if let Some(b) = v.get("gitBranch").and_then(|x| x.as_str()) {
                if !b.is_empty() {
                    act.git_branch = Some(b.to_string());
                }
            }
        }

        // ended_at — max event timestamp; also feeds active_secs (R6/D4).
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if let Some(unix) = parse_iso_utc(ts) {
                act.ended_at = Some(act.ended_at.map_or(unix, |cur| cur.max(unix)));
                event_times.push(unix);
            }
        }

        // file-history-snapshot — the authoritative edited set (LAST wins).
        if v.get("type").and_then(|x| x.as_str()) == Some("file-history-snapshot") {
            if let Some(map) = v
                .get("snapshot")
                .and_then(|s| s.get("trackedFileBackups"))
                .and_then(|t| t.as_object())
            {
                act.edited_paths = map.keys().cloned().collect();
            }
        }

        let role = v
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(|x| x.as_str());

        // First-user-prompt candidates.
        if role == Some("user") {
            let is_meta = v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false);
            let is_sidechain = v
                .get("isSidechain")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let source = v.get("promptSource").and_then(|x| x.as_str());
            if let Some(text) = user_message_text(&v) {
                if source == Some("typed") {
                    if typed_prompt.is_none() {
                        typed_prompt = Some(truncate_chars(&text, PROMPT_PREVIEW_MAX_CHARS));
                    }
                } else if fallback_prompt.is_none()
                    && !is_meta
                    && !is_sidechain
                    && !is_wrapper_text(&text)
                {
                    fallback_prompt = Some(truncate_chars(&text, PROMPT_PREVIEW_MAX_CHARS));
                }
                // V0029/user_turns — the honest "real prompts" count: every
                // Real-classified, non-sidechain turn, not just the FIRST one
                // (the typed/fallback slots above only ever keep one).
                if !is_sidechain
                    && matches!(
                        classify_user_text(&text, is_meta, source),
                        UserTextClass::Real
                    )
                {
                    user_turns += 1;
                }
            }

            // S9 — decisions (AskUserQuestion answers, plan approvals) + errors.
            extract_decisions_and_errors(&v, &mut act.decisions, &mut act.error_count);
            // P5 — pair a git Bash result back to its command + extract the SHA.
            extract_commits(&v, &mut pending_git, &mut act.commits);
            // W0.2 — the SYNCHRONOUS subagent-stats fallback (parent
            // transcript's own toolUseResult; the sidecar walk is W0.4/W0.5).
            extract_subagent_stats(&v, &mut act);
            // W4/R8/ADD-2 — pair a grokclaude Bash result back to its command
            // and recover the job ulid, if confidently present.
            extract_grok_job(&v, &mut pending_grok, &mut act.research, &mut research_seen);
        }

        // tool-call file touches (assistant content blocks).
        if role == Some("assistant") {
            // S9 — model (last wins) + token usage.
            if let Some(m) = v
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(|x| x.as_str())
            {
                if !m.is_empty() {
                    act.model = Some(m.to_string());
                }
            }
            if let Some(usage) = v.get("message").and_then(|m| m.get("usage")) {
                let inp = usage
                    .get("input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                let out = usage
                    .get("output_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                act.token_total = act.token_total.saturating_add(inp).saturating_add(out);
            }
            // R3/V0029 — closure tracking: same last-wins substantial/any
            // pair as the standalone `closing_assistant_text`.
            if let Some(text) = closing_candidate_text(&v) {
                if text.chars().count() >= CLOSING_TEXT_SUBSTANTIAL_MIN_CHARS {
                    closing_substantial = Some(text.clone());
                }
                closing_any = Some(text);
            }
            if let Some(blocks) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for b in blocks {
                    if b.get("type").and_then(|x| x.as_str()) != Some("tool_use") {
                        continue;
                    }
                    act.tool_calls += 1;
                    let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("");
                    // P5 — a git commit/push/tag Bash call: remember it by id so
                    // its result (with the SHA) can be paired back.
                    if name == "Bash" {
                        if let Some(cmd) = b
                            .get("input")
                            .and_then(|i| i.get("command"))
                            .and_then(|x| x.as_str())
                        {
                            let git_actions = git_action_of(cmd);
                            if !git_actions.is_empty() {
                                if let Some(id) = b.get("id").and_then(|x| x.as_str()) {
                                    pending_git
                                        .entry(id.to_string())
                                        .or_default()
                                        .extend(git_actions);
                                }
                            }
                            // W4/R8/ADD-2 — a Bash call that invokes grokclaude
                            // (research/session/build/panel/fleet): remember it
                            // by id, awaiting its result for the job ulid.
                            if grok_job_invocation_regex().is_match(cmd) {
                                if let Some(id) = b.get("id").and_then(|x| x.as_str()) {
                                    pending_grok
                                        .insert(id.to_string(), truncate_chars(cmd.trim(), 160));
                                }
                            }
                        }
                    }
                    // R4 — research / tool-usage signals (detected, not ground
                    // truth #10), deduped on (kind, query).
                    if let Some(r) = classify_research(name, b.get("input")) {
                        if research_seen.insert(r.clone()) {
                            act.research.push(r);
                        }
                    }
                    let Some(action) = tool_file_action(name) else {
                        continue;
                    };
                    let input = b.get("input");
                    let path = input
                        .and_then(|i| i.get("file_path"))
                        .or_else(|| input.and_then(|i| i.get("notebook_path")))
                        .and_then(|x| x.as_str());
                    if let Some(p) = path {
                        if !p.is_empty() {
                            let touch = FileTouch {
                                path: p.to_string(),
                                action,
                            };
                            if files_seen.insert(touch.clone()) {
                                act.files.push(touch);
                            }
                        }
                    }
                }
            }
        }
    }

    act.message_count = count;
    act.first_user_prompt = typed_prompt.or(fallback_prompt);
    act.cwd = cwd_counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(c, _)| c);
    act.user_turns = user_turns;
    act.last_assistant_text = closing_substantial.or(closing_any);
    act.active_secs = active_secs(&event_times);
    act.skipped_lines = skipped_lines;
    if skipped_lines > 0 {
        tracing::warn!(
            skipped_lines,
            "session activity parse dropped unparseable JSONL lines"
        );
    }
    (act, transcript_agent_id)
}

// --- P1/V0029: project-key derivation ---------------------------------------

/// P1/V0029 — derive `(project_key, repo_root)` from already-parsed signals:
/// **rung 2**, the modal `repo_root` over RESOLVED rows of the envelope's
/// `kb-session-commits` tail block (already on disk for every capture that
/// committed — `extract_commits_block`), else **rung 3**, the modal `cwd`
/// (`repo_root` stays `None`: a bare cwd is not a confirmed git root).
/// `project_key = claude_project_slug(root)` either way.
///
/// **Rung 1** (a forward-capture `kb-session-project` tail block written by
/// `kb sessions capture` at commit time — design Proposal 2c/D6) is a capture
/// engine change and is deliberately OUT of this wave's scope: rungs 2+3
/// already resolve every session that ever committed (the majority of
/// project-scoped work) plus every other session via its cwd, and rung 1 can
/// be layered in later as a pure ADDITION (it would only ever promote a
/// rung-3 row to rung-1, never change an already-correct rung-2 answer).
///
/// Ties (two repo roots with the same commit count) break lexicographically
/// smallest — deterministic, golden-pinned.
pub fn derive_project(
    cwd: Option<&str>,
    commits: &[CapturedCommit],
) -> (Option<String>, Option<String>) {
    let mut root_counts: Vec<(String, u32)> = Vec::new();
    for c in commits {
        if !c.resolved {
            continue;
        }
        let Some(root) = c
            .repo_root
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        else {
            continue;
        };
        match root_counts.iter_mut().find(|(r, _)| r == root) {
            Some(slot) => slot.1 += 1,
            None => root_counts.push((root.to_string(), 1)),
        }
    }
    if let Some(root) = modal_tie_break(&root_counts) {
        return (
            Some(crate::session_bundle::claude_project_slug(&root)),
            Some(root),
        );
    }
    match cwd.map(str::trim).filter(|c| !c.is_empty()) {
        Some(cwd) => (Some(crate::session_bundle::claude_project_slug(cwd)), None),
        None => (None, None),
    }
}

/// Highest count wins; ties break lexicographically smallest. `None` on an
/// empty input.
fn modal_tie_break(counts: &[(String, u32)]) -> Option<String> {
    counts
        .iter()
        .min_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)))
        .map(|(s, _)| s.clone())
}

/// R5/V0029 — harness ladder rung 2: `<meta name="kb-harness" content="…">`
/// in the envelope head. A lightweight substring scan (mirrors `extract_pre`'s
/// style — cheap, no full HTML parse), since only the codex/opencode adapters
/// emit this tag today and only when rung 1 (the `adapter-meta` JSONL line,
/// [`SessionActivity::harness`]) is, for some reason, absent.
pub(crate) fn meta_harness(html: &str) -> Option<String> {
    let marker = r#"<meta name="kb-harness" content=""#;
    let start = html.find(marker)? + marker.len();
    let end = html[start..].find('"')? + start;
    let val = html[start..end].trim();
    (!val.is_empty()).then(|| val.to_string())
}

/// S1/V0029 — the deterministic substance ladder (surfaces.md S1), rule-
/// provable from already-parsed signals only:
///
/// * `trivial` — zero non-wrapper assistant text AND zero tool calls (the
///   `/clear` husks — a session with nothing to show for itself).
/// * `routine` — no edits, no commits, and fewer than 4 real turns
///   ([`SessionActivity::user_turns`]).
/// * `substantive` — everything else.
///
/// The surfaces.md ladder also names "no memories" as a `routine` condition;
/// that signal (`memory_count`) is a CROSS-KB count computed at HTTP-request
/// time (`count_docs_with_kb_session` scans every corpus for docs whose
/// `kb_session` matches), not a pure function of this capture's bytes — an
/// unrelated later `kb remember` elsewhere in the corpus would silently flip
/// a session's substance on reindex despite the capture itself being
/// unchanged, which breaks reproducibility (#11's ethos: a capture's derived
/// facts are a function of ITS bytes). Dropped from the ladder here;
/// substance is computed from transcript-local signals only.
pub fn session_substance(
    has_assistant_text: bool,
    tool_calls: u32,
    files_edited: u32,
    commit_count: u32,
    user_turns: u32,
) -> &'static str {
    if !has_assistant_text && tool_calls == 0 {
        "trivial"
    } else if files_edited == 0 && commit_count == 0 && user_turns < 4 {
        "routine"
    } else {
        "substantive"
    }
}

/// Char-boundary-safe head-truncation with a trailing `…` marker when the
/// input was actually cut. Used for [`LAST_ASSISTANT_TEXT_MAX_CHARS`] (the
/// persisted column) and [`OUTCOME_WIRE_MAX_CHARS`] (the wire preview) so a
/// truncated closing message reads as visibly cut, never as a suspiciously
/// abrupt full stop.
pub fn truncate_chars_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out.push('…');
    out
}

// --- R3: the closure extractor ----------------------------------------------

/// A closing text at least this long (chars) is "substantial" — prose that
/// reads as an answer rather than a connective ("Now the index line:",
/// "Running the tests."). The floor picks the LAST substantial closing over a
/// later one-liner, mirroring how [`parse_session_activity`]'s typed pass wins
/// over the wrapper-skipping fallback regardless of position.
pub(crate) const CLOSING_TEXT_SUBSTANTIAL_MIN_CHARS: usize = 40;

/// Assistant text the harness synthesised on the model's behalf — an
/// interruption marker, never the assistant's own closure. Complements the
/// typed signals (`isApiErrorMessage`, `model:"<synthetic>"`); the observed
/// `[Request interrupted …]` records are USER-role (so the assistant-only scan
/// already skips them), but a future harness build could attribute one to the
/// assistant and this keeps the answer honest either way.
const SYNTHETIC_ASSISTANT_MARKERS: &[&str] = &["[Request interrupted", "[Request cancelled"];

/// R3 — the deterministic CLOSURE of a session: the last real assistant prose
/// in the transcript, the peer of [`SessionActivity::first_user_prompt`].
///
/// The whole rule (LLM-free, clock-free, deterministic given the bytes) is a
/// REVERSE scan for the last eligible assistant text — implemented as one
/// forward pass with two last-wins slots, which is the same answer:
///
/// 1. **substantial** (preferred): the last eligible text of at least
///    [`CLOSING_TEXT_SUBSTANTIAL_MIN_CHARS`] chars — the closing summary.
/// 2. **fallback**: the last eligible text of ANY length, so a terse session
///    ("Done.") still reports a closure instead of `None`.
///
/// A record is eligible when it is a MAIN-THREAD (`isSidechain != true`,
/// `isMeta != true`) assistant message carrying non-empty `text` blocks, and
/// it is NOT harness-synthesised (`isApiErrorMessage`, `model:"<synthetic>"`
/// — the "You've hit your monthly spend limit" / "API Error: …" records) and
/// not a synthetic wrapper ([`is_wrapper_text`],
/// [`SYNTHETIC_ASSISTANT_MARKERS`]). tool_use-only and thinking-only records
/// carry no `text` block, so the requestId-shatter tail (a session whose last
/// event is an empty thinking fragment) resolves to the last real prose.
///
/// Returns the text UNTRUNCATED — callers cap it
/// ([`LAST_ASSISTANT_TEXT_MAX_CHARS`] for the persisted column,
/// [`OUTCOME_WIRE_MAX_CHARS`] for the wire preview). `None` when the
/// transcript holds no assistant prose at all (a husk capture: `[mode]` +
/// caveat + `/clear`).
pub fn closing_assistant_text(jsonl: &str) -> Option<String> {
    let mut substantial: Option<String> = None;
    let mut any: Option<String> = None;
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(text) = closing_candidate_text(&v) else {
            continue;
        };
        if text.chars().count() >= CLOSING_TEXT_SUBSTANTIAL_MIN_CHARS {
            substantial = Some(text.clone());
        }
        any = Some(text);
    }
    substantial.or(any)
}

/// Fold the R3 substantial/any last-wins rule over an ordered stream of
/// already-extracted prose candidates (used by `session-view/1` to derive
/// the outcome from the walk's Assistant Prose items without a second full
/// JSONL re-parse). Wrapper/synthetic-marker text is rejected the same way
/// [`closing_candidate_text`] rejects it after extraction.
pub(crate) fn closing_text_from_prose_candidates<'a>(
    texts: impl Iterator<Item = &'a str>,
) -> Option<String> {
    let mut substantial: Option<String> = None;
    let mut any: Option<String> = None;
    for text in texts {
        if is_wrapper_text(text)
            || SYNTHETIC_ASSISTANT_MARKERS
                .iter()
                .any(|m| text.starts_with(m))
        {
            continue;
        }
        // Image placeholders the view injects are never a closure.
        if text == "[image attachment]" {
            continue;
        }
        if text.chars().count() >= CLOSING_TEXT_SUBSTANTIAL_MIN_CHARS {
            substantial = Some(text.to_string());
        }
        any = Some(text.to_string());
    }
    substantial.or(any)
}

/// One record's contribution to [`closing_assistant_text`]: its joined
/// assistant `text` blocks when the record is an eligible main-thread,
/// non-synthetic assistant message, else `None`.
fn closing_candidate_text(v: &serde_json::Value) -> Option<String> {
    let msg = v.get("message")?;
    if msg.get("role").and_then(|x| x.as_str()) != Some("assistant") {
        return None;
    }
    if v.get("isSidechain").and_then(|x| x.as_bool()) == Some(true)
        || v.get("isMeta").and_then(|x| x.as_bool()) == Some(true)
    {
        return None;
    }
    // Harness-synthesised assistant records (rate limits, stream stalls,
    // model-unavailable notices) are not the assistant's closure.
    if v.get("isApiErrorMessage").and_then(|x| x.as_bool()) == Some(true)
        || msg.get("model").and_then(|x| x.as_str()) == Some("<synthetic>")
    {
        return None;
    }
    let text = assistant_message_text(msg)?;
    if is_wrapper_text(&text)
        || SYNTHETIC_ASSISTANT_MARKERS
            .iter()
            .any(|m| text.starts_with(m))
    {
        return None;
    }
    Some(text)
}

/// The prose of an assistant message: its `content` string, or the joined
/// `type:"text"` blocks (STRICTLY those — `thinking` blocks carry their text
/// under `thinking`, and a tool_use block has no prose at all). Trimmed;
/// `None` when empty.
fn assistant_message_text(msg: &serde_json::Value) -> Option<String> {
    let content = msg.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(|x| x.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|x| x.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// --- R6/D4: honest active time ----------------------------------------------

/// R6/D4 — the honest ACTIVE duration (seconds) of a session: the sum over
/// consecutive-event timestamp deltas, each clamped to
/// [`ACTIVE_DELTA_CLAMP_SECS`]. A gap longer than the clamp (the operator
/// walked away; a multi-day capture accreted across Stops) contributes the
/// clamp, not its wall-clock length, so a 6-day session no longer reports
/// "169h 59m" of work. Out-of-order / duplicate timestamps contribute 0 —
/// transcripts are never sorted (the replay grammar's rule), so a clock skew
/// can shrink but never inflate the answer.
///
/// `event_times` is the transcript-ordered list of event timestamps in unix
/// seconds. Pure and deterministic; the ONE fn both the parse row and the
/// reader header consume, so the list and the header can't disagree.
pub fn active_secs(event_times: &[i64]) -> i64 {
    event_times.windows(2).fold(0i64, |acc, w| {
        let delta = w[1].saturating_sub(w[0]).clamp(0, ACTIVE_DELTA_CLAMP_SECS);
        acc.saturating_add(delta)
    })
}

/// Map a tool name to the file action it implies, or `None` when the tool
/// doesn't touch a single named file (Bash/Grep/Glob/Agent/…).
fn tool_file_action(name: &str) -> Option<FileAction> {
    match name {
        "Read" => Some(FileAction::Read),
        "Write" => Some(FileAction::Write),
        "Edit" | "MultiEdit" | "NotebookEdit" => Some(FileAction::Edit),
        _ => None,
    }
}

/// Pull a user message's text out of its `content` (string OR a block array
/// whose `text` blocks we join). Returns `None` when there's no text (e.g.
/// a tool_result-only user turn).
fn user_message_text(v: &serde_json::Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|x| x.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// S9 — extract steering decisions (AskUserQuestion answers, plan approvals)
/// and tally error tool-results from one user event. Prefers the STRUCTURED
/// `toolUseResult.answers` map (immune to prose wording flips); plan approvals
/// + the error tally come from the tool_result blocks.
fn extract_decisions_and_errors(
    v: &serde_json::Value,
    decisions: &mut Vec<Decision>,
    error_count: &mut u32,
) {
    if let Some(answers) = v
        .get("toolUseResult")
        .and_then(|t| t.get("answers"))
        .and_then(|a| a.as_object())
    {
        // Stable order via questions[]; fall back to the map's order.
        let questions = v
            .get("toolUseResult")
            .and_then(|t| t.get("questions"))
            .and_then(|q| q.as_array());
        let mut pushed = false;
        if let Some(qs) = questions {
            for q in qs {
                if let Some(qt) = q.get("question").and_then(|x| x.as_str()) {
                    if let Some(a) = answers.get(qt) {
                        decisions.push(Decision {
                            kind: "question".into(),
                            prompt: qt.to_string(),
                            answer: Some(answer_to_string(a)),
                        });
                        pushed = true;
                    }
                }
            }
        }
        if !pushed {
            for (q, a) in answers {
                decisions.push(Decision {
                    kind: "question".into(),
                    prompt: q.clone(),
                    answer: Some(answer_to_string(a)),
                });
            }
        }
    }
    if let Some(blocks) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    {
        for b in blocks {
            if b.get("type").and_then(|x| x.as_str()) != Some("tool_result") {
                continue;
            }
            if b.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false) {
                *error_count += 1;
            }
            if tool_result_text(b)
                .trim_start()
                .starts_with("User has approved your plan")
            {
                decisions.push(Decision {
                    kind: "plan".into(),
                    prompt: "plan approved".into(),
                    answer: None,
                });
            }
        }
    }
}

/// P5 — classify a Bash command as VCS actions, returning `(kind, subject)`
/// for EVERY matched git segment in the command — a chained `git commit …
/// && git push` yields BOTH a commit event and a push event, not just the
/// first hit. Recognises `git commit` (subject = the `-m`/`-F` message, when
/// recoverable — see [`extract_commit_message`]), `git push`, `git tag
/// <name>` (create). Skips read-only git (`git log`, `git status`, `git
/// diff`, bare `git tag` / `git tag -l` / `git tag --list`, …).
fn git_action_of(cmd: &str) -> Vec<(String, Option<String>)> {
    let mut actions = Vec::new();
    // Look at each &&/;/| -separated segment so `cd x && git commit …`
    // matches, AND so a chained command is classified segment-by-segment
    // instead of stopping at the first hit.
    for seg in cmd.split(['&', ';', '|', '\n']) {
        let s = seg.trim();
        let Some(rest) = s.strip_prefix("git ") else {
            continue;
        };
        let rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("commit") {
            // Only a real `git commit` (bare, or `commit -m …` / `commit
            // --amend`) — NOT `commit-tree` / `commit-graph` (the next char
            // after "commit" must be whitespace or end-of-segment, not `-x`).
            if after.is_empty() || after.starts_with(char::is_whitespace) {
                let subject = match extract_commit_message(after, cmd) {
                    // No `-m`/`-F` flag at all — a generic label, not a
                    // fabricated subject.
                    None => Some("commit".to_string()),
                    // A message flag IS present but unrecoverable from the
                    // transcript (plain `-F <file>`) — flagged for
                    // capture-time resolution later; never invent a subject.
                    Some(None) => None,
                    Some(Some(s)) => Some(s),
                };
                actions.push(("commit".to_string(), subject));
            }
            continue;
        }
        if rest == "push" || rest.starts_with("push ") {
            actions.push(("push".to_string(), Some("push".to_string())));
            continue;
        }
        if rest == "tag" {
            // Bare `git tag` (no args) lists existing tags — a read.
            continue;
        }
        if let Some(tail) = rest.strip_prefix("tag ") {
            let tail = tail.trim_start();
            let is_list = tail == "-l"
                || tail.starts_with("-l ")
                || tail == "--list"
                || tail.starts_with("--list ");
            if is_list {
                // `git tag -l[ist] […]` — a read, not a write.
                continue;
            }
            actions.push(("tag".to_string(), Some(rest.to_string())));
        }
    }
    actions
}

/// Pull the commit subject out of a `git commit` tail, when a message flag
/// (`-m` or `-F`) is present. Three-way result distinguishes "no flag at
/// all" from "flag present but text unrecoverable":
/// - `None` — no `-m`/`-F` flag at all; the caller falls back to a generic
///   `"commit"` label (not a fabricated subject).
/// - `Some(None)` — a message flag IS present but the subject text isn't
///   recoverable from the transcript (plain `-F <file>`: the message lives
///   in a file kb never sees). Flagged for capture-time resolution later —
///   NEVER invent a subject here.
/// - `Some(Some(subject))` — the recovered subject text.
///
/// `full_cmd` is the ORIGINAL, un-split multi-line Bash command (`cmd` in
/// `git_action_of`) — `after_commit` itself only ever holds the segment's
/// first line (the caller splits on `\n` before this is reached), which is
/// enough for a plain `-m "subject"` but not for a heredoc body that spans
/// several lines.
fn extract_commit_message(after_commit: &str, full_cmd: &str) -> Option<Option<String>> {
    if let Some(idx) = after_commit.find("-m") {
        let tail = after_commit[idx + 2..].trim_start();
        // Heredoc form: `-m "$(cat <<'EOF' … EOF)"` (agents commonly wrap long,
        // multi-line commit messages this way). Bash substitutes the heredoc
        // BODY for the `$(...)` before `-m` ever sees it, so the real subject is
        // the body's first non-empty line — not the literal `$(cat <<'EOF'`
        // text that's all `after_commit`'s single line contains. Look the body
        // up in `full_cmd`, which still has the newlines.
        if let Some(subject) = extract_heredoc_subject(tail, full_cmd) {
            return Some(Some(truncate_chars(&subject, 120)));
        }
        let msg = if let Some(q) = tail.strip_prefix('"') {
            q.split('"').next().unwrap_or("")
        } else if let Some(q) = tail.strip_prefix('\'') {
            q.split('\'').next().unwrap_or("")
        } else {
            tail.split_whitespace().next().unwrap_or("")
        };
        let msg = msg.trim();
        return Some((!msg.is_empty()).then(|| truncate_chars(msg, 120)));
    }
    if let Some(idx) = after_commit.find("-F") {
        let tail = after_commit[idx + 2..].trim_start();
        // `-F -` reads the message from STDIN — recoverable ONLY when the
        // Bash command itself redirects a heredoc onto stdin (`git commit -F
        // - <<'EOF' … EOF`), which shows up as a literal `<<` right after the
        // `-F -` token; the heredoc body's first non-empty line is the
        // subject (same extraction as `-m`'s `$(cat <<EOF…)` form). Plain
        // `-F <file>` is UNRECOVERABLE — the message text lives in a file kb
        // never sees — so no subject is invented; it's flagged (`Some(None)`)
        // for capture-time resolution later.
        if let Some(subject) = extract_heredoc_subject(tail, full_cmd) {
            return Some(Some(truncate_chars(&subject, 120)));
        }
        return Some(None);
    }
    None
}

/// Parse the heredoc delimiter word out of a `<<[-]['"]DELIM['"]` fragment on
/// `tail` (the `-m` argument's first line), then look its BODY up in
/// `full_cmd` and return the body's first non-empty line. Handles both
/// quoted delimiter forms (`<<'EOF'`, `<<"EOF"`) and the bare/unquoted one
/// (`<<EOF`); `<<-` (indent-stripping heredocs) is also recognised. `None`
/// when `tail` isn't a heredoc at all, or the delimiter's terminator line is
/// found before any body content.
fn extract_heredoc_subject(tail: &str, full_cmd: &str) -> Option<String> {
    let after_op = tail.find("<<")?;
    let mut rest = &tail[after_op + 2..];
    rest = rest.strip_prefix('-').unwrap_or(rest); // <<- indented form
    let rest = rest.trim_start();
    let rest = rest
        .strip_prefix('\'')
        .or_else(|| rest.strip_prefix('"'))
        .unwrap_or(rest);
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let delim = &rest[..end];
    if delim.is_empty() {
        return None;
    }
    // Skip to (and past) the line that opens the heredoc — the body starts
    // on the line right after it — then take the first non-empty body line,
    // stopping if the terminator is hit first.
    let mut lines = full_cmd.lines();
    for line in lines.by_ref() {
        if line.contains("<<") && line.contains(delim) {
            break;
        }
    }
    for line in lines {
        if line.trim() == delim {
            return None;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// R4 — classify one `tool_use` block as a research / tool-usage signal, or
/// `None` when it isn't one. Version-tolerant `.get().and_then()` reads; the
/// extracted query is best-effort. "Detected, not ground truth" (#10).
fn classify_research(name: &str, input: Option<&serde_json::Value>) -> Option<Research> {
    let field = |k: &str| -> Option<String> {
        input
            .and_then(|i| i.get(k))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| truncate_chars(t, 160))
    };
    let item = |kind: &str, query: String| {
        Some(Research {
            kind: kind.to_string(),
            query,
        })
    };
    match name {
        "WebSearch" => item("web", field("query").unwrap_or_default()),
        "WebFetch" => item(
            "web",
            field("prompt")
                .or_else(|| field("query"))
                .or_else(|| field("url"))
                .unwrap_or_default(),
        ),
        // Subagent-delegation tool: named "Task" historically, renamed
        // "Agent" in newer transcripts (both seen in the wild — GC-B8, the
        // subagent-iceberg defect). Add future renames to this pattern only.
        "Task" | "Agent" => item(
            "subagent",
            field("description")
                .or_else(|| field("prompt"))
                .or_else(|| field("subagent_type"))
                .unwrap_or_default(),
        ),
        "Skill" => item(
            "skill",
            field("command")
                .or_else(|| field("name"))
                .or_else(|| field("skill"))
                .unwrap_or_default(),
        ),
        "ExitPlanMode" => item(
            "plan_span",
            field("plan")
                .map(|p| truncate_chars(p.lines().next().unwrap_or("").trim(), 120))
                .unwrap_or_default(),
        ),
        "Bash" => {
            let cmd = input
                .and_then(|i| i.get("command"))
                .and_then(|x| x.as_str())?;
            kb_cli_query(cmd).map(|(kind, query)| Research {
                kind: kind.to_string(),
                query,
            })
        }
        // MCP tools surface as `mcp__<server>__<tool>`. RA2 — Playwright /
        // browser-automation MCP calls are UI scaffolding, not research; a
        // session that drove the browser shouldn't read as "researched: …".
        n if n.starts_with("mcp__") => {
            if n.contains("playwright") || n.contains("browser") {
                None
            } else {
                item("skill", n.to_string())
            }
        }
        _ => None,
    }
}

/// R4 — detect a kb-CLI RESEARCH-family invocation in a Bash command and pull
/// out `(kind, "<verb> <query>")`. Reuses git_action_of's segment scan (so
/// `cd x && kb search "foo"` matches) and tolerates a leading `env`/`VAR=val`
/// prefix. Best-effort, version-tolerant.
///
/// RA2 — only the genuine *exploration* verbs count as research. The
/// self-referential memory verbs (`recall`/`recollect`/`why`/`remember`) are
/// deliberately EXCLUDED: the `/kb-reflect` dream-loop itself runs
/// `kb recall`/`kb remember`, and counting those as "research" would poison the
/// very digests it reads (and inflate the activity funnel).
///
/// CT-A5 — `ARTIFACT_OPEN_VERBS` are a SEPARATE `artifact_open` kind (session_research
/// CHECK constraint V0020/V0030), wiring the activity funnel's `opened` stage
/// (`sqlite.rs::sessions_funnel_counts`) to a real Bash-detected producer
/// instead of `session_files` reads alone.
fn kb_cli_query(cmd: &str) -> Option<(&'static str, String)> {
    const VERBS: &[&str] = &["search", "find", "related"];
    // Real kb-cli artifact-dump verbs (checked against crates/kb-cli/src/main.rs
    // — there is no literal `open` verb; `kb read` is the browser/xdg-open one
    // and is driven by the OS, not a captured Bash tool call, so it never
    // reaches this parser). NOISE CAVEAT: an agent re-reading its own
    // just-written artifact mid-session (a `kb cat`/`kb get` self-read during
    // capture) counts here too and will inflate the funnel's `opened` stage —
    // "detected, not ground truth" (#10). If that self-read noise ends up
    // dominating, the recorded fallback is dropping the `opened` stage
    // rather than trying to distinguish self-reads from genuine opens.
    const ARTIFACT_OPEN_VERBS: &[&str] = &["cat", "get"];
    for seg in cmd.split(['&', ';', '|', '\n']) {
        // Strip a leading `env ` and any `VAR=val` assignments before `kb`.
        let mut s = seg.trim();
        loop {
            let t = s.trim_start();
            if let Some(rest) = t.strip_prefix("env ") {
                s = rest;
                continue;
            }
            if let Some(sp) = t.find(char::is_whitespace) {
                let first = &t[..sp];
                if first.contains('=') && !first.starts_with('-') {
                    s = &t[sp..];
                    continue;
                }
            }
            s = t;
            break;
        }
        let Some(rest) = s.strip_prefix("kb ") else {
            continue;
        };
        let rest = rest.trim_start();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let verb = parts.next().unwrap_or("");
        let kind = if VERBS.contains(&verb) {
            "kb_search"
        } else if ARTIFACT_OPEN_VERBS.contains(&verb) {
            "artifact_open"
        } else {
            continue;
        };
        let query = first_query_arg(parts.next().unwrap_or(""));
        return Some((kind, format!("{verb} {query}").trim().to_string()));
    }
    None
}

/// R4 — the first positional/quoted argument of a kb-CLI verb tail, skipping
/// flags. A quoted query wins; otherwise the first non-`-` token.
fn first_query_arg(tail: &str) -> String {
    let t = tail.trim_start();
    if let Some(q) = t.strip_prefix('"') {
        return truncate_chars(q.split('"').next().unwrap_or("").trim(), 120);
    }
    if let Some(q) = t.strip_prefix('\'') {
        return truncate_chars(q.split('\'').next().unwrap_or("").trim(), 120);
    }
    for tok in t.split_whitespace() {
        if tok.starts_with('-') {
            continue;
        }
        return truncate_chars(tok, 120);
    }
    String::new()
}

/// P5 — for each tool_result in a user event whose tool_use_id is a pending
/// git command, pair it back to the git action(s) that command produced (one
/// Bash call can chain several — `commit && push` — so the pending value is
/// a `Vec`) and parse a SHA from the shared output. Deduped on `(kind, sha)`
/// so a retried or echoed command doesn't yield two rows for the same
/// detected event.
fn extract_commits(
    v: &serde_json::Value,
    pending_git: &mut std::collections::BTreeMap<String, Vec<(String, Option<String>)>>,
    commits: &mut Vec<Commit>,
) {
    let Some(blocks) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return;
    };
    for b in blocks {
        if b.get("type").and_then(|x| x.as_str()) != Some("tool_result") {
            continue;
        }
        let Some(id) = b.get("tool_use_id").and_then(|x| x.as_str()) else {
            continue;
        };
        if let Some(actions) = pending_git.remove(id) {
            let sha = parse_git_sha(&tool_result_text(b));
            for (kind, subject) in actions {
                // Dedup on (kind, sha) ONLY when a sha was recovered — two
                // distinct commits whose tool results both failed to yield a
                // parseable sha must not collapse into one (`sha: None` is
                // "unknown", not an identity).
                let dup = sha.is_some() && commits.iter().any(|c| c.kind == kind && c.sha == sha);
                if dup {
                    continue;
                }
                commits.push(Commit {
                    kind,
                    sha: sha.clone(),
                    subject,
                });
            }
        }
    }
}

/// W4/R8/ADD-2 — a Bash command that invokes the `grokclaude` CLI's job
/// verbs. Mirrors the child-side extraction rule the memo specifies verbatim
/// (`grokclaude\s+(research|session|build|panel|fleet)\b`) — the SAME regex
/// on both sides of the join, so a driver-side row and a future child-side
/// row (W5) can never disagree about what counts as "invoked grokclaude".
fn grok_job_invocation_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"grokclaude\s+(research|session|build|panel|fleet)\b")
            .expect("valid grokclaude invocation regex")
    })
}

/// W4/R8/ADD-2 — a grokclaude job ulid: 26-char Crockford base32 (`0-9`,
/// `A-HJKMNP-TV-Z` — no `I`/`L`/`O`/`U`), UPPERCASE only (grokclaude's own
/// `ulid::Ulid::new().to_string()` — verified live: `src/id.rs:17` — never
/// lowercases it, so a lowercase run is never treated as a candidate; being
/// case-strict here is part of "confidently a job id", not an oversight).
fn grok_job_ulid_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\b[0-9A-HJKMNP-TV-Z]{26}\b").expect("valid grok job ulid regex")
    })
}

/// W4/R8/ADD-2 — for each tool_result in a user event whose tool_use_id is a
/// pending grokclaude invocation, pair it back and record ONE `session_research`
/// row (`kind="grok_job"`). The query is the job ulid when EXACTLY ONE
/// distinct ulid-shaped token appears in the result text (conservative: zero
/// or ambiguous (>1 distinct) matches fall back to the invoking command line,
/// per the memo's "only when confidently a job id"). Dedup mirrors every
/// other `research` push site (insertion-ordered Vec + HashSet membership).
fn extract_grok_job(
    v: &serde_json::Value,
    pending_grok: &mut std::collections::BTreeMap<String, String>,
    research: &mut Vec<Research>,
    research_seen: &mut std::collections::HashSet<Research>,
) {
    let Some(blocks) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return;
    };
    for b in blocks {
        if b.get("type").and_then(|x| x.as_str()) != Some("tool_result") {
            continue;
        }
        let Some(id) = b.get("tool_use_id").and_then(|x| x.as_str()) else {
            continue;
        };
        let Some(cmd) = pending_grok.remove(id) else {
            continue;
        };
        let text = tool_result_text(b);
        let mut ulids: Vec<&str> = grok_job_ulid_regex()
            .find_iter(&text)
            .map(|m| m.as_str())
            .collect();
        ulids.sort_unstable();
        ulids.dedup();
        let query = match ulids.as_slice() {
            [one] => one.to_string(),
            _ => cmd,
        };
        let r = Research {
            kind: "grok_job".to_string(),
            query,
        };
        if research_seen.insert(r.clone()) {
            research.push(r);
        }
    }
}

/// W5/R10 — conservative extraction of TaskOutput truncation marker paths
/// from a raw transcript (or sidecar text): only the ONE known shape a
/// backgrounded Bash tool's truncated output emits — `Full output:
/// <path ending in .../tasks/<id>.output>` (verified live against real
/// captures, both `/tmp/claude-<uid>/<project>/<sid>/tasks/<id>.output` and
/// the `/var/tmp/...` equivalent). Deliberately does NOT match the
/// unrelated `<persisted-output>…Full output saved to: …/tool-results/
/// <id>.txt` marker (a different, generic large-tool-result mechanism) —
/// conservative by design, per the milestone's "only paths matching the
/// known tasks-output shapes" rule. Order-preserved, deduplicated; an empty
/// or marker-less text yields an empty `Vec`, never a false positive.
pub fn task_output_paths(text: &str) -> Vec<String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"Full output:\s*(\S+/tasks/[A-Za-z0-9._-]+\.output)\]?")
            .expect("valid task-output marker regex")
    });
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for caps in re.captures_iter(text) {
        let p = caps[1].to_string();
        if seen.insert(p.clone()) {
            out.push(p);
        }
    }
    out
}

/// W0.2 — subagent aggregate stats, the SYNCHRONOUS parent-transcript
/// fallback. Ground truth (verified against real transcripts, not guessed):
/// an Agent/Task delegation's `toolUseResult` — sync OR async — always
/// carries `agentId`, so that key alone is the signal; it needs no
/// correlation back to the originating `tool_use` block. A completed
/// delegation additionally carries `totalTokens` / `totalToolUseCount` /
/// `toolStats.editFileCount` (the real field names, from
/// `subagent-delegation.jsonl` + `ask-user-question.jsonl`); an
/// ASYNC/BACKGROUND delegation (the harness default —
/// `tool-heavy-research.jsonl`) leaves only a `status:"async_launched"` stub
/// with none of those. Classifying on the PRESENCE of `totalTokens` (rather
/// than matching the literal `status` string) means a future status value
/// still lands in the right bucket without a new match arm. "Detected, not
/// ground truth" (#10 ethos) — the real numbers for an unstatted launch live
/// only in the per-agent sidecar files W0.4/W0.5 read at capture time.
fn extract_subagent_stats(v: &serde_json::Value, act: &mut SessionActivity) {
    let Some(result) = v.get("toolUseResult") else {
        return;
    };
    let Some(_agent_id) = result.get("agentId").and_then(|x| x.as_str()) else {
        return;
    };
    match result.get("totalTokens").and_then(|x| x.as_u64()) {
        Some(tokens) => {
            act.subagent_count += 1;
            act.subagent_tokens = act.subagent_tokens.saturating_add(tokens);
            if let Some(calls) = result.get("totalToolUseCount").and_then(|x| x.as_u64()) {
                act.subagent_tool_calls = act.subagent_tool_calls.saturating_add(calls as u32);
            }
            if let Some(edited) = result
                .get("toolStats")
                .and_then(|t| t.get("editFileCount"))
                .and_then(|x| x.as_u64())
            {
                act.subagent_files_edited = act.subagent_files_edited.saturating_add(edited as u32);
            }
        }
        None => {
            act.subagent_launched_unstatted += 1;
        }
    }
}

/// Best-effort SHA extraction from git output. `git commit` prints
/// `[branch abc1234] subject`; `git push`/`tag` echo short hashes. Returns the
/// first 7–40 hex token (skipping ones that are all-decimal, e.g. counts).
fn parse_git_sha(output: &str) -> Option<String> {
    // Prefer the `[branch <sha>]` form from `git commit`.
    if let Some(open) = output.find('[') {
        if let Some(close) = output[open..].find(']') {
            let inside = &output[open + 1..open + close];
            if let Some(tok) = inside.split_whitespace().find(|t| is_hex_sha(t)) {
                return Some(tok.to_string());
            }
        }
    }
    output
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|t| is_hex_sha(t))
        .map(|t| t.to_string())
}

/// A 7–40 char hex token that isn't all decimal digits (so a line/byte count
/// like `1234567` isn't mistaken for a SHA — requires at least one a–f).
fn is_hex_sha(t: &str) -> bool {
    let len = t.len();
    (7..=40).contains(&len)
        && t.bytes().all(|b| b.is_ascii_hexdigit())
        && t.bytes().any(|b| b.is_ascii_alphabetic())
}

/// Stringify an AskUserQuestion answer value: a string verbatim, an array
/// comma-joined (multiSelect), else its JSON form.
fn answer_to_string(a: &serde_json::Value) -> String {
    match a {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// Extract a tool_result block's textual content (string, or the joined
/// `text`/`content` of an array form).
fn tool_result_text(b: &serde_json::Value) -> String {
    match b.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|it| {
                it.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| it.as_str())
                    .map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// True when the text is a synthetic command/hook wrapper rather than a real
/// typed prompt. Used only in the fallback pass (old transcripts without
/// `promptSource`). R14/W1 — delegates to [`wrapper_kind`], the same
/// classification the `session-view/1` engine's interpretation catalog uses,
/// so the two can never drift; output-neutral (the set of texts this returns
/// `true` for is byte-identical to the old inline `WRAPPERS` list — every
/// `wrapper_kind` prefix is one of the old list's six, `LocalCommandOther`
/// included since it's still `<local-command-`-prefixed).
fn is_wrapper_text(text: &str) -> bool {
    wrapper_kind(text).is_some()
}

// --- R14: the shared user-text classifier -----------------------------------

/// R14 — how one `role:"user"` transcript text should be treated: genuinely
/// human-authored prose, a harness-synthesised turn, or a recognised
/// synthetic wrapper envelope. Shared by BOTH consumers the memo names:
/// [`parse_session_activity`]'s two-pass first-prompt rule (via
/// [`is_wrapper_text`], unchanged output) and the `session-view/1` engine's
/// item interpretation + outline/TLDR extraction (NEW this wave — this is the
/// actual R14 fix: the OLD renderer's header TLDR took the literal first user
/// text with no wrapper skip at all, so a `<local-command-caveat>` or
/// `<task-notification>` block could become the headline quote).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTextClass {
    /// Real user-authored prose — either `promptSource=="typed"` (the gold
    /// signal on modern transcripts) or text that clears the wrapper-skip
    /// fallback (old transcripts predating `promptSource`).
    Real,
    /// A harness-synthesised turn (`isMeta`) — never shown as if the human
    /// wrote it.
    Meta,
    /// A recognised synthetic wrapper envelope, with its specific shape.
    Wrapper(WrapperKind),
}

/// The specific synthetic wrapper envelope a `role:"user"` text carries. Each
/// variant is a distinct interpretation-catalog treatment in the
/// `session-view/1` engine (P2's table); [`is_wrapper_text`]'s boolean use
/// only needs `Some(_)` vs `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapperKind {
    /// `<command-name>`/`<command-message>`/`<command-args>` — a slash
    /// command's own expansion.
    Command,
    /// `<local-command-stdout>` — a slash command's captured output.
    LocalCommandStdout,
    /// `<local-command-caveat>` — CLI boilerplate, suppressed by default.
    LocalCommandCaveat,
    /// Any other `<local-command-…>` envelope not specifically recognised
    /// above (forward-compat: still a wrapper, just not further classified).
    LocalCommandOther,
    /// `<task-notification>` — a joined task-lifecycle event.
    TaskNotification,
    /// `<system-reminder>` — hook/system context, including recall-hook
    /// memory-injection payloads.
    SystemReminder,
    /// `<bash-stdout>` / `<bash-stderr>` — inline shell echo wrappers.
    BashStdio,
}

/// Classify one wrapper prefix, or `None` for ordinary text. The specific
/// checks (`<local-command-caveat` / `<local-command-stdout`) are tried
/// BEFORE the generic `<local-command-` fallthrough so both are distinct
/// [`WrapperKind`]s while the union of everything this matches is exactly the
/// old `is_wrapper_text` prefix list.
fn wrapper_kind(text: &str) -> Option<WrapperKind> {
    let t = text.trim_start();
    if t.starts_with("<local-command-caveat") {
        Some(WrapperKind::LocalCommandCaveat)
    } else if t.starts_with("<local-command-stdout") {
        Some(WrapperKind::LocalCommandStdout)
    } else if t.starts_with("<local-command-") {
        Some(WrapperKind::LocalCommandOther)
    } else if t.starts_with("<command-") {
        Some(WrapperKind::Command)
    } else if t.starts_with("<task-notification>") {
        Some(WrapperKind::TaskNotification)
    } else if t.starts_with("<system-reminder>") {
        Some(WrapperKind::SystemReminder)
    } else if t.starts_with("<bash-stdout>") || t.starts_with("<bash-stderr>") {
        Some(WrapperKind::BashStdio)
    } else {
        None
    }
}

/// R14 — the shared classifier. `is_meta` wins over everything (a synthetic
/// turn is never "real" even if its text happens to look typed);
/// `promptSource=="typed"` is the gold signal on modern transcripts; anything
/// else falls through the wrapper check, and clears to [`UserTextClass::Real`]
/// when it isn't a recognised wrapper — matching `is_wrapper_text`'s old
/// fallback-pass gate exactly (that gate additionally checks `!is_sidechain`,
/// which callers apply separately since it's about turn ATTRIBUTION, not
/// envelope classification).
pub fn classify_user_text(text: &str, is_meta: bool, prompt_source: Option<&str>) -> UserTextClass {
    if is_meta {
        return UserTextClass::Meta;
    }
    if prompt_source == Some("typed") {
        return UserTextClass::Real;
    }
    match wrapper_kind(text) {
        Some(kind) => UserTextClass::Wrapper(kind),
        None => UserTextClass::Real,
    }
}

// --- session-id resolution ---------------------------------------------------

fn meta_session(html: &str) -> Option<String> {
    let f = crate::parser::extract(html);
    f.kb_session
}

/// Parse `session-<YYYYMMDDTHHMMSSZ>-<sid>.html` (the kb-capture.sh
/// wrapper). Returns the session-id portion (third capture).
fn filename_session_id(filename: &str) -> Option<String> {
    let stem = file_stem(filename)?;
    let caps = session_filename_regex().captures(stem)?;
    Some(caps.get(2)?.as_str().to_string())
}

fn filename_timestamp(filename: &str) -> Option<i64> {
    let stem = file_stem(filename)?;
    let caps = session_filename_regex().captures(stem)?;
    let ts = caps.get(1)?.as_str();
    parse_compact_utc(ts)
}

fn filename_stem(filename: &str) -> String {
    file_stem(filename)
        .map(str::to_string)
        .unwrap_or_else(|| filename.to_string())
}

fn file_stem(filename: &str) -> Option<&str> {
    let bare = filename.rsplit('/').next().unwrap_or(filename);
    bare.strip_suffix(".html").or(Some(bare))
}

fn session_filename_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"^session-(\d{8}T\d{6}Z)-(.+)$").expect("valid session filename regex")
    })
}

// `YYYYMMDDTHHMMSSZ` → unix seconds; `None` on a format glitch (the
// caller falls back to `mtime_unix`). Shared parser — see timeparse.rs.
use crate::timeparse::{parse_compact_utc, parse_iso_utc};

// --- transcript text extraction ----------------------------------------------

/// Extract the first `<pre>…</pre>` block's inner text. The
/// kb-capture.sh wrapper writes exactly one `<pre>` containing the
/// HTML-escaped JSONL transcript; we don't need a full HTML parser
/// for this. Returns `None` when no `<pre>` block exists.
fn extract_pre(html: &str) -> Option<String> {
    let open = html.find("<pre>")?;
    let after = &html[open + "<pre>".len()..];
    let close = after.find("</pre>")?;
    Some(after[..close].to_string())
}

/// Reverse of kb-capture.sh's `sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g'
/// -e 's/>/\&gt;/g'`. Order matters: `&amp;` is matched as a full entity
/// in one left-to-right pass (equivalent to the historical chained
/// replace with `&amp;` last), so `&amp;lt;` decodes to the two chars
/// `&lt;` rather than further to `<`.
fn html_unescape(s: &str) -> String {
    html_unescape_entities(s, &["lt", "gt", "amp"])
}

/// One-pass entity decode into a pre-sized buffer. `names` is the entity
/// body list (without `&`/`;`) in first-match priority order — match the
/// historical chained-`.replace` sequence so `&amp;lt;` / `&amp;amp;`
/// stay byte-identical to the old multi-pass form.
fn html_unescape_entities(s: &str, names: &[&str]) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            let rest = &s[i + 1..];
            let mut matched = false;
            for name in names {
                if let Some(after) = rest.strip_prefix(name) {
                    if after.starts_with(';') {
                        match *name {
                            "lt" => out.push('<'),
                            "gt" => out.push('>'),
                            "amp" => out.push('&'),
                            "quot" => out.push('"'),
                            "#39" => out.push('\''),
                            _ => unreachable!("entity name not in match arms: {name}"),
                        }
                        i += 1 + name.len() + 1;
                        matched = true;
                        break;
                    }
                }
            }
            if matched {
                continue;
            }
        }
        let ch = s[i..].chars().next().expect("i < len");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Recover the **byte-identical** raw JSONL transcript from a capture artifact
/// (the `kb-capture.sh` / `kb import claude-history` envelope): the first
/// `<pre>…</pre>` inner text, HTML-unescaped in the mandatory `&amp;`-last
/// order. `None` when the HTML carries no `<pre>` (not a capture).
///
/// This is the canonical inverse of the capture escape — the ONE place the
/// round-trip lives, so `kb sessions export` (and the daemon export route) can
/// reconstruct a transcript that `claude -r` reads without a byte of drift.
/// Do NOT substitute `session_render`'s 5-entity unescape: that is a superset
/// (adds `&quot;`/`&#39;`) and is not the minimal inverse of the hook's chain.
pub fn recover_jsonl_from_capture(html: &str) -> Option<String> {
    extract_pre(html).map(|raw| html_unescape(&raw))
}

// --- capture-time commit resolution block (W0.4) ----------------------------

/// The `<script>` id of the envelope's additive commits block (see
/// [`render_commits_block`] / [`extract_commits_block`]). A sibling of the
/// `<pre>` transcript, written by `kb sessions capture` AFTER `</pre>` so the
/// pre-block stays byte-identical to the historical `kb-capture.sh` heredoc
/// (`recover_jsonl_from_capture`'s round-trip invariant, #11) — this block is
/// purely additive and absent whenever there's nothing to add (no commits
/// detected, or an old/imported capture that predates capture-time
/// resolution).
pub const COMMITS_BLOCK_ID: &str = "kb-session-commits";

/// One commit record as it rides the envelope's additive JSON block —
/// transcript-detected `kind`/`sha`/`subject` (kept even when resolution
/// failed, so a rebased-away sha still shows a best-effort subject) plus the
/// capture-time git resolution (`sha_full`/`repo_root`/`author`/`parents`/
/// `trailers`, all `None`/empty unless `resolved`). Field names mirror the
/// `session_commits` (V0019/V0025) DB row verbatim — `SessionCaptureHook`
/// maps this struct straight onto `SessionCommitRow`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapturedCommit {
    /// `"commit" | "push" | "tag"`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha_full: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parents: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trailers: Vec<String>,
}

/// Render the additive commits block for the envelope tail, or `""` when
/// `commits` is empty (the split-contract rule: absent, not an empty array,
/// when there's nothing to add — `kb import claude-history` backfills and
/// commit-less sessions never emit the tag at all).
///
/// `</` is escaped to `<\/` inside the embedded JSON (solidus-escaping is
/// valid JSON) so a commit subject that happens to contain the literal text
/// `</script>` can never prematurely close the block; `extract_commits_block`
/// needs no matching unescape since `serde_json` already treats `\/` as `/`.
pub fn render_commits_block(commits: &[CapturedCommit]) -> String {
    if commits.is_empty() {
        return String::new();
    }
    let json = serde_json::to_string(commits).unwrap_or_else(|_| "[]".to_string());
    let escaped = json.replace("</", "<\\/");
    format!(r#"<script type="application/json" id="{COMMITS_BLOCK_ID}">{escaped}</script>"#)
}

/// Parse the additive commits block out of a capture's HTML, when present.
/// `None` when the block is absent OR malformed — never a hard failure, so
/// `SessionCaptureHook` falls back to transcript-only detection
/// (`resolved: false` on every row) rather than losing the capture.
pub fn extract_commits_block(html: &str) -> Option<Vec<CapturedCommit>> {
    let marker = format!(r#"id="{COMMITS_BLOCK_ID}""#);
    let id_at = html.find(&marker)?;
    let tag_end = html[id_at..].find('>')? + id_at + 1;
    let close_at = html[tag_end..].find("</script>")? + tag_end;
    serde_json::from_str(html[tag_end..close_at].trim()).ok()
}

// --- CT-F1: the `Kb-Memory:` trailer parse-back -----------------------------

/// CT-F1 — the commit-trailer key `plugins/kb-memory/hooks/git-dispatch/
/// trailer-logic.sh` stamps (one line per memory minted in the committing
/// session), the memory-side sibling of W0.3's `Kb-Session:`. Compared
/// case-INSENSITIVELY by [`memory_ids_from_trailers`]: git treats trailer
/// keys case-insensitively and a hand-written `kb-memory:` is the same
/// claim.
pub const MEMORY_TRAILER_KEY: &str = "Kb-Memory";

/// CT-F1 — the outcome of parsing one commit's trailer block for
/// [`MEMORY_TRAILER_KEY`] lines. `malformed` is a CENSUS, not an error
/// channel: a `Kb-Memory:` line whose value isn't a bare 12-hex id is
/// counted and dropped, never guessed at and never stored, so a future
/// grammar drift is greppable in the capture log instead of silently
/// zeroing the ledger (the same posture `derive_memory_recalls`'s
/// three-way census takes for the recall marker).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryTrailerParse {
    /// Distinct memory ids, in first-seen trailer order.
    pub ids: Vec<String>,
    /// `Kb-Memory:` lines whose value did not parse as a 12-hex id.
    pub malformed: usize,
}

/// CT-F1 — read the `Kb-Memory: <hex12>` ids back out of one commit's
/// VERBATIM unfolded trailer lines (`vcs::resolve_commit`'s `trailers`,
/// persisted since V0025 in `session_commits.trailers` and carried on the
/// capture envelope's `CapturedCommit::trailers`). Pure, LLM-free, no git
/// call, no I/O — the whole CT-F1 read side is a projection of bytes the
/// capture already had.
///
/// The grammar is deliberately CLOSED, exactly like the `kb-recall/1`
/// marker's: `<key>: <value>` where the key matches
/// [`MEMORY_TRAILER_KEY`] case-insensitively and the value is EXACTLY 12
/// lowercase hex digits. Anything else — an uppercase or short id, a
/// `<hex12> extra` suffix, an empty value — is counted in
/// [`MemoryTrailerParse::malformed`] and dropped: an id is an exact join
/// key, so a *maybe* is worth less than an honest absence. Duplicates
/// collapse (a `--amend` can legitimately re-stamp the same id).
pub fn memory_ids_from_trailers(trailers: &[String]) -> MemoryTrailerParse {
    let mut out = MemoryTrailerParse::default();
    for line in trailers {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case(MEMORY_TRAILER_KEY) {
            continue;
        }
        let value = value.trim();
        let is_hex12 = value.len() == 12
            && value
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        if !is_hex12 {
            out.malformed += 1;
            continue;
        }
        if !out.ids.iter().any(|id| id == value) {
            out.ids.push(value.to_string());
        }
    }
    out
}

// --- capture-time subagent file-grain digest (W0.5) -------------------------

/// The `<script>` id of the envelope's additive subagent digest block (see
/// [`render_subagents_block`] / [`extract_subagents_block`]). A sibling of
/// the `<pre>` transcript and the [`COMMITS_BLOCK_ID`] block, written by `kb
/// sessions capture` / `kb import claude-history --refresh-subagents` AFTER
/// `</pre>` so the pre-block stays byte-identical to the historical
/// `kb-capture.sh` heredoc — purely additive, absent whenever the session's
/// `<session-id>/subagents/` directory has no `agent-*.jsonl` sidecars.
pub const SUBAGENTS_BLOCK_ID: &str = "kb-session-subagents";

/// Hard cap on the rendered `<script id="kb-session-subagents">` block —
/// [`render_subagents_block`] trims the largest per-agent file lists first
/// (never drops a whole agent) until the block fits, and stamps
/// `truncated: true` the moment any trimming happens.
const SUBAGENTS_BLOCK_CAP_BYTES: usize = 256 * 1024;

/// One file a subagent touched, as it rides the digest block. Plain strings
/// (not [`FileTouch`]/[`FileAction`]) — the block is a wire/JSON shape, not
/// an in-memory parse result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentFileEntry {
    /// Verbatim path from the subagent's own transcript.
    pub path: String,
    /// `"read" | "write" | "edit"` (see [`FileAction::as_str`]).
    pub action: String,
}

/// One subagent's digest, parsed from its sidecar transcript
/// (`<session-id>/subagents/agent-*.jsonl`) via [`parse_subagent_jsonl`].
/// "Detected, not ground truth" (#10 ethos) — same caveat as every other
/// transcript-derived count in this module.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SubagentDigest {
    /// The subagent's own id — the transcript's own `agentId` field when
    /// present (ground truth), else the `agent-<id>.jsonl` filename stem.
    pub agent_id: String,
    /// Distinct `(path, action)` file touches, in first-seen order (mirrors
    /// [`SessionActivity::files`]'s dedup).
    #[serde(default)]
    pub files: Vec<SubagentFileEntry>,
    /// Summed `input_tokens + output_tokens` over the sidecar's own
    /// assistant turns.
    #[serde(default)]
    pub tokens: u64,
    /// Count of `tool_use` blocks in the sidecar's own assistant turns.
    #[serde(default)]
    pub tool_calls: u32,
    /// Count of `is_error` tool-results in the sidecar's own transcript.
    #[serde(default)]
    pub errors: u32,
}

/// The envelope's additive subagent digest block — every sidecar the capture
/// walked, plus whether the file-list cap trimmed anything.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SubagentsBlock {
    #[serde(default)]
    pub agents: Vec<SubagentDigest>,
    /// `true` when [`SUBAGENTS_BLOCK_CAP_BYTES`] forced at least one
    /// per-agent file list to be trimmed.
    #[serde(default)]
    pub truncated: bool,
}

/// Parse one sidecar transcript's raw JSONL into its [`SubagentDigest`].
/// Reuses [`parse_session_activity`] wholesale — a subagent's own transcript
/// carries the SAME line shapes (`message.role`, `tool_use` blocks, `usage`,
/// `is_error` tool-results) as the main transcript, so the token/tool-call/
/// error totals and the distinct file-touch set fall out of the existing
/// parser for free rather than a forked copy.
///
/// `agent_id` prefers a literal top-level `"agentId"` field found anywhere in
/// the sidecar (ground truth, when Claude Code stamps the subagent's own
/// lines with it); `filename_agent_id` — the caller's `agent-<id>.jsonl`
/// stem — is the fallback, since the on-disk filename is itself a reliable
/// identifier for which agent this sidecar belongs to.
pub fn parse_subagent_jsonl(jsonl: &str, filename_agent_id: &str) -> SubagentDigest {
    // One walk: activity + agentId ride the same `parse_session_activity_full`
    // pass (no second line-scan for `transcript_agent_id`).
    let (act, agent_id) = parse_session_activity_full(jsonl);
    let agent_id = agent_id.unwrap_or_else(|| filename_agent_id.to_string());
    SubagentDigest {
        agent_id,
        files: act
            .files
            .iter()
            .map(|f| SubagentFileEntry {
                path: f.path.clone(),
                action: f.action.as_str().to_string(),
            })
            .collect(),
        tokens: act.token_total,
        tool_calls: act.tool_calls,
        errors: act.error_count,
    }
}

/// Walk one session's sidecar directory (`<session-id>/subagents/`) for
/// `agent-*.jsonl` transcripts and parse each into a [`SubagentDigest`].
/// Non-recursive by construction: `subagents/workflows/**` (Workflow-tool
/// journals, not agent transcripts — same exclusion `kb import
/// claude-history`'s main walk already relies on) sits in a SIBLING
/// subdirectory, so a plain directory listing filtered to `agent-*.jsonl`
/// files never descends into it — no explicit depth/path check needed.
/// Best-effort: an unreadable sidecar is skipped, never a hard error.
/// Returns an empty `Vec` when `dir` doesn't exist or has no matching files
/// (the caller then omits the digest block entirely, same "absent when
/// empty" contract as [`render_commits_block`]).
pub fn collect_subagent_digests(dir: &Path) -> Vec<SubagentDigest> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("agent-") && n.ends_with(".jsonl"))
        })
        .collect();
    // Deterministic order — the digest block's agent order shouldn't depend
    // on the OS's directory-listing order.
    files.sort();
    let mut out = Vec::with_capacity(files.len());
    let mut skipped = 0u32;
    for path in &files {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let filename_id = stem.strip_prefix("agent-").unwrap_or(stem);
                out.push(parse_subagent_jsonl(&raw, filename_id));
            }
            Err(e) => {
                skipped += 1;
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "skipping unreadable subagent sidecar"
                );
            }
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            dir = %dir.display(),
            "subagent sidecar collection skipped unreadable files"
        );
    }
    out
}

/// Render the additive subagent digest block for the envelope tail, or `""`
/// when `agents` is empty (same split-contract rule as
/// [`render_commits_block`]: absent, not an empty array). Caps the rendered
/// block at [`SUBAGENTS_BLOCK_CAP_BYTES`], trimming the largest per-agent
/// file lists first — see [`cap_subagents_block`].
///
/// `</` is escaped to `<\/` inside the embedded JSON, same reasoning as
/// `render_commits_block` (a file path that happens to contain the literal
/// text `</script>` can never prematurely close the block).
pub fn render_subagents_block(agents: &[SubagentDigest]) -> String {
    if agents.is_empty() {
        return String::new();
    }
    let block = cap_subagents_block(SubagentsBlock {
        agents: agents.to_vec(),
        truncated: false,
    });
    let json = serde_json::to_string(&block).unwrap_or_else(|_| "{}".to_string());
    let escaped = json.replace("</", "<\\/");
    format!(r#"<script type="application/json" id="{SUBAGENTS_BLOCK_ID}">{escaped}</script>"#)
}

/// Trim `block` until its rendered JSON fits [`SUBAGENTS_BLOCK_CAP_BYTES`],
/// popping files from whichever agent currently has the MOST (never
/// emptying one agent while a smaller one still holds files), and stamping
/// `truncated = true` the moment any trimming happens. A fast pass tracks a
/// per-entry size ESTIMATE (avoids re-serializing the whole block on every
/// popped file — O(n) instead of O(n²) on a pathologically large sidecar
/// set); a bounded finishing pass re-checks with a real serialize so the
/// result is never actually over cap because of estimate drift.
fn cap_subagents_block(mut block: SubagentsBlock) -> SubagentsBlock {
    if render_len(&block) <= SUBAGENTS_BLOCK_CAP_BYTES {
        return block;
    }
    block.truncated = true;
    let mut size = render_len(&block);
    while size > SUBAGENTS_BLOCK_CAP_BYTES {
        let Some(agent) = block.agents.iter_mut().max_by_key(|a| a.files.len()) else {
            break;
        };
        let Some(popped) = agent.files.pop() else {
            break;
        };
        let entry_len = serde_json::to_string(&popped)
            .map(|s| s.len() + 1)
            .unwrap_or(1);
        size = size.saturating_sub(entry_len);
    }
    while render_len(&block) > SUBAGENTS_BLOCK_CAP_BYTES {
        let Some(agent) = block.agents.iter_mut().max_by_key(|a| a.files.len()) else {
            break;
        };
        if agent.files.pop().is_none() {
            break;
        }
    }
    block
}

fn render_len(block: &SubagentsBlock) -> usize {
    serde_json::to_string(block).map(|s| s.len()).unwrap_or(0)
}

/// Parse the additive subagent digest block out of a capture's HTML, when
/// present. `None` when the block is absent OR malformed — never a hard
/// failure, so `SessionCaptureHook` falls back to parent-transcript-only
/// subagent stats (the pre-W0.5 behavior) rather than losing the capture.
pub fn extract_subagents_block(html: &str) -> Option<SubagentsBlock> {
    let marker = format!(r#"id="{SUBAGENTS_BLOCK_ID}""#);
    let id_at = html.find(&marker)?;
    let tag_end = html[id_at..].find('>')? + id_at + 1;
    let close_at = html[tag_end..].find("</script>")? + tag_end;
    serde_json::from_str(html[tag_end..close_at].trim()).ok()
}

/// Remove any existing block matching `id` (a full `<script …>…</script>`
/// tag, plus one trailing newline when present) wherever it sits in `html`.
/// A no-op when the id isn't present. Shared by both digest blocks
/// ([`COMMITS_BLOCK_ID`] / [`SUBAGENTS_BLOCK_ID`]) so `kb import
/// claude-history --refresh-subagents` can rewrite the subagents block
/// in-place, idempotently, without disturbing anything else in the tail.
fn strip_script_block(html: &str, id: &str) -> String {
    let marker = format!(r#"<script type="application/json" id="{id}">"#);
    let Some(start) = html.find(&marker) else {
        return html.to_string();
    };
    let Some(end_rel) = html[start..].find("</script>") else {
        return html.to_string();
    };
    let mut tail_start = start + end_rel + "</script>".len();
    if html[tail_start..].starts_with('\n') {
        tail_start += 1;
    }
    format!("{}{}", &html[..start], &html[tail_start..])
}

/// Rewrite the subagents digest block in an EXISTING capture's HTML: strip
/// whatever subagents block is already present (if any — makes repeated
/// refreshes idempotent) and splice the freshly-rendered one in right before
/// `</body>`, leaving the `<pre>` transcript and every other block (e.g. the
/// commits block) byte-for-byte untouched. `agents` empty ⇒ the block is
/// removed and nothing is re-added (mirrors "absent when empty").
/// `kb import claude-history --refresh-subagents`'s backfill path.
pub fn replace_subagents_block(html: &str, agents: &[SubagentDigest]) -> String {
    let stripped = strip_script_block(html, SUBAGENTS_BLOCK_ID);
    let block = render_subagents_block(agents);
    if block.is_empty() {
        return stripped;
    }
    match stripped.rfind("</body>") {
        Some(at) => format!("{}{}\n{}", &stripped[..at], block, &stripped[at..]),
        None => format!("{stripped}{block}\n"),
    }
}

// --- capture-time subagent sidecar TEXT block (W0.6) ------------------------

/// The container element id of the envelope's additive sidecar-text tail
/// block (see [`render_sidecar_text_block`] / [`extract_sidecar_text_block`]
/// / [`replace_sidecar_text_block`]). A sibling of the `<pre>` transcript,
/// the [`COMMITS_BLOCK_ID`] block, and the [`SUBAGENTS_BLOCK_ID`] digest —
/// written AFTER the subagents digest so the envelope's tail reads
/// digest-then-evidence. Deliberately a `<section hidden>`, NOT a `<script
/// type="application/json">` like its siblings: `parser::body_text`
/// (SKIP_TAGS = script/style/template/noscript) walks INTO
/// `<section>`/`<details>`/`<pre>` but never into `<script>`, so this is
/// the one tail block whose content the indexer's BM25/vector pass actually
/// sees — the JSON digest blocks stay structured-only and invisible to
/// search on purpose (#11 R1: the DIGEST is the searchable surface for the
/// main transcript; this block is raw sidecar EVIDENCE riding alongside
/// it, not a resumable transcript — truncation is acceptable). Purely
/// additive and absent whenever the session has no sidecars, same "absent
/// when empty" contract as the digest blocks.
pub const SIDECAR_TEXT_BLOCK_ID: &str = "kb-session-sidecar-text";

/// Per-agent cap on RAW (pre-escape) sidecar JSONL bytes folded into the
/// [`SIDECAR_TEXT_BLOCK_ID`] block. An agent whose sidecar exceeds this
/// keeps a HEAD/TAIL slice — see [`truncate_and_escape_agent_raw`].
pub const SIDECAR_TEXT_AGENT_CAP_BYTES: usize = 2 * 1024 * 1024;

/// Total cap on RAW (pre-escape) sidecar JSONL bytes across ALL agents in
/// one [`SIDECAR_TEXT_BLOCK_ID`] block. [`sidecar_agent_budgets`] charges
/// each agent (in `agent_id` sort order) for what it actually USES, so an
/// agent under its own [`SIDECAR_TEXT_AGENT_CAP_BYTES`] leaves headroom for
/// agents later in sort order rather than wasting a fixed per-slot ration —
/// this cap is a true ceiling on total RAW bytes ONLY, not rendered ones:
/// [`truncate_and_escape_agent_raw`] truncates THEN HTML-escapes, so the
/// `<pre>` bytes that actually land in the block can exceed this cap by
/// however much the `&`/`<`/`>` entity expansion inflates the kept text.
pub const SIDECAR_TEXT_TOTAL_CAP_BYTES: usize = 8 * 1024 * 1024;

/// Refusal threshold for a RAW transcript entering capture/import whole.
/// The main-transcript `<pre>` is a byte-identical resume contract, so an
/// oversized transcript is REFUSED, never truncated (2026-08-21: a 292MB
/// raw Codex rollout — 77% base64 screenshots — bypassed the harness
/// adapter and OOM-looped the ci-host daemon; the largest legitimate capture
/// to date is 32MB).
pub const CAPTURE_MAX_TRANSCRIPT_BYTES: u64 = 48 * 1024 * 1024;

/// Outcome of checking a RAW transcript's on-disk byte length against
/// [`CAPTURE_MAX_TRANSCRIPT_BYTES`] BEFORE it is read into memory whole.
/// Pure — both RAW capture entrypoints (`kb sessions capture`'s `capture`,
/// `kb import claude-history`'s per-file loop) apply this to
/// `fs::metadata(..).len()` and act on the verdict instead of duplicating
/// the size arithmetic: `Refuse` means the caller must not read the file
/// (capture errors out; import skips it with a warning), `Proceed` means
/// either the file is within cap or `--allow-oversized` was passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptSizeVerdict {
    Proceed,
    Refuse,
}

/// See [`TranscriptSizeVerdict`]. `cap` is threaded (not hardcoded to
/// [`CAPTURE_MAX_TRANSCRIPT_BYTES`]) so tests can exercise the refuse/allow
/// branches at a human-scale size instead of writing multi-megabyte
/// fixtures.
pub fn transcript_size_verdict(len: u64, cap: u64, allow_oversized: bool) -> TranscriptSizeVerdict {
    if len > cap && !allow_oversized {
        TranscriptSizeVerdict::Refuse
    } else {
        TranscriptSizeVerdict::Proceed
    }
}

/// Render the additive sidecar-text tail block for the envelope, or `None`
/// when `agents` is empty (same split-contract rule as
/// [`render_commits_block`]/[`render_subagents_block`]: absent, not an
/// empty container). `agents` is `(agent_id, raw_jsonl)` pairs — RAW,
/// pre-escape sidecar file contents; this fn sorts by `agent_id` (document
/// order must not depend on the caller's / OS's directory-listing order,
/// same reasoning as [`collect_subagent_digests`]), budgets each agent via
/// [`sidecar_agent_budgets`], truncates+escapes via
/// [`truncate_and_escape_agent_raw`], and wraps each agent in a
/// `<details data-kb-sidecar-agent="…"><summary>…</summary><pre>…</pre></details>`
/// inside the single [`SIDECAR_TEXT_BLOCK_ID`] `<section hidden>` container.
pub fn render_sidecar_text_block(agents: &[(String, String)]) -> Option<String> {
    if agents.is_empty() {
        return None;
    }
    let mut sorted: Vec<&(String, String)> = agents.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let lens: Vec<usize> = sorted.iter().map(|(_, raw)| raw.len()).collect();
    let budgets = sidecar_agent_budgets(&lens);

    let mut parts = String::new();
    for ((agent_id, raw), budget) in sorted.iter().zip(budgets) {
        let attr = escape_sidecar_agent_id(agent_id);
        let body = truncate_and_escape_agent_raw(raw, budget);
        parts.push_str(&format!(
            r#"<details data-kb-sidecar-agent="{attr}"><summary>{attr}</summary><pre>{body}</pre></details>"#
        ));
    }
    Some(format!(
        r#"{}<section id="{SIDECAR_TEXT_BLOCK_ID}" hidden>{parts}</section>"#,
        sidecar_unhide_style_tag(SIDECAR_TEXT_BLOCK_ID)
    ))
}

/// R14 — the sidecar deep-link un-hide affordance: a companion `<style>` that
/// makes the `hidden` [`SIDECAR_TEXT_BLOCK_ID`] section un-hide itself when
/// the URL fragment targets it directly (`?raw=1#kb-session-sidecar-text`,
/// the link the rendered subagents footer's per-agent rows emit). CSS-only —
/// `[hidden]` normally wins over any URL fragment landing, so without this a
/// deep link to the raw page's sidecar evidence scrolls to nothing visible.
/// No JS, no route change: the raw page is served byte-for-byte (scrubbed)
/// from the captured envelope, so the affordance has to ride the envelope
/// itself. Purely additive — same "absent when empty" contract as the block
/// it sits beside; [`strip_section_block`] strips it back out together with
/// the section it targets.
fn sidecar_unhide_style_tag(id: &str) -> String {
    format!(r#"<style>#{id}:target{{display:block}}</style>"#)
}

/// Deterministic per-agent byte budget for [`render_sidecar_text_block`]:
/// agents in `raw_lens` order (the caller has already sorted by
/// `agent_id`) each get `min(SIDECAR_TEXT_AGENT_CAP_BYTES,
/// remaining_total)`, and `remaining_total` is charged for what that agent
/// ACTUALLY uses (`raw_len.min(budget)`), not the full assigned budget —
/// see [`SIDECAR_TEXT_TOTAL_CAP_BYTES`]. Pure over lengths only (no
/// MB-sized fixtures needed to unit-test the math).
fn sidecar_agent_budgets(raw_lens: &[usize]) -> Vec<usize> {
    let mut remaining = SIDECAR_TEXT_TOTAL_CAP_BYTES;
    raw_lens
        .iter()
        .map(|&len| {
            let budget = SIDECAR_TEXT_AGENT_CAP_BYTES.min(remaining);
            remaining = remaining.saturating_sub(len.min(budget));
            budget
        })
        .collect()
}

/// Pure budget-math check for whether [`render_sidecar_text_block`] would
/// HEAD/TAIL-truncate ANY agent's raw sidecar text — the SAME sort-by-
/// `agent_id` + [`sidecar_agent_budgets`] walk the render fn itself uses, so
/// the two can never disagree, but returns a `bool` instead of building the
/// rendered HTML. Exists so a caller that only needs the FLAG (e.g. `kb
/// sessions capture`'s `CaptureSummary::sidecar_text_truncated`) doesn't have
/// to substring-scan the rendered artifact for
/// [`sidecar_truncation_marker`]'s marker text — which false-positives
/// whenever the MAIN transcript legitimately contains that literal string
/// (e.g. a session that worked on this very sidecar-text feature). `false`
/// for an empty `agents` slice (nothing to truncate).
pub fn sidecar_text_truncates(agents: &[(String, String)]) -> bool {
    if agents.is_empty() {
        return false;
    }
    let mut sorted: Vec<&(String, String)> = agents.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let lens: Vec<usize> = sorted.iter().map(|(_, raw)| raw.len()).collect();
    let budgets = sidecar_agent_budgets(&lens);
    lens.iter().zip(budgets).any(|(&len, budget)| len > budget)
}

/// Pure HEAD/TAIL byte-count split of an over-budget agent's `budget`: 60%
/// head, 40% tail (rounding folds into the tail so `head + tail == budget`
/// exactly) — no UTF-8 awareness, byte counts only, so this unit-tests as
/// plain arithmetic. [`truncate_and_escape_agent_raw`] snaps both ends to
/// the nearest char boundary before slicing.
fn sidecar_head_tail_split(budget: usize) -> (usize, usize) {
    let head = budget * 3 / 5;
    (head, budget - head)
}

/// The one fixed truncation marker line spliced between an over-budget
/// agent's kept HEAD and TAIL halves, reporting the exact number of RAW
/// bytes dropped. Deterministic text — pinned by tests.
fn sidecar_truncation_marker(dropped_bytes: usize) -> String {
    format!("\n[kb-sidecar-text: truncated {dropped_bytes} bytes]\n")
}

/// Largest byte index `<= idx` that lands on a UTF-8 char boundary of `s`
/// (safe to slice `&s[..that]`). Hand-rolled — the stdlib equivalent
/// (`str::floor_char_boundary`) is nightly-only; same walk-back idiom
/// `parser::extract`'s prompt-length cap already uses.
///
/// `pub(crate)` so `sessions::view` (and any other in-crate scanner that
/// caps a UTF-8 slice mid-string) can reuse the same snap rather than
/// forking a second walk-back.
pub(crate) fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest byte index `>= idx` that lands on a UTF-8 char boundary of `s`
/// (safe to slice `&s[that..]`). Pairs with [`floor_char_boundary`] for the
/// TAIL half of an over-budget truncation — the head side floors down, the
/// tail side ceils up, so neither slice starts or ends mid-character.
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Truncate one agent's RAW sidecar bytes to fit `budget`, THEN HTML-escape
/// — never the other way round, or a multi-character entity (`&amp;`)
/// straddling the cut point would be sliced in half. Under budget: escaped
/// verbatim, untruncated. Over budget: keeps the HEAD 60% / TAIL 40% of
/// `budget` (see [`sidecar_head_tail_split`]), each end snapped to the
/// nearest UTF-8 char boundary, joined by [`sidecar_truncation_marker`]
/// reporting the exact dropped byte count.
fn truncate_and_escape_agent_raw(raw: &str, budget: usize) -> String {
    if raw.len() <= budget {
        return escape_sidecar_text(raw);
    }
    let (head_len, tail_len) = sidecar_head_tail_split(budget);
    let head_end = floor_char_boundary(raw, head_len);
    let tail_start = ceil_char_boundary(raw, raw.len() - tail_len).max(head_end);
    let dropped = tail_start - head_end;
    let marker = sidecar_truncation_marker(dropped);
    let kept = format!("{}{marker}{}", &raw[..head_end], &raw[tail_start..]);
    escape_sidecar_text(&kept)
}

/// HTML-escape RAW sidecar text before embedding in a `<pre>` — the SAME
/// three-entity scheme as the main transcript's `<pre>` (`kb-capture.sh`'s
/// `sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'`, order
/// load-bearing so a literal `&` isn't re-escaped). Pairs with the shared
/// [`html_unescape`] for [`extract_sidecar_text_block`]'s round trip.
/// Single pass into a capacity-hinted buffer (no intermediate `String`s).
fn escape_sidecar_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// HTML-attribute escape for the sidecar-text block's `agent_id` (used both
/// as the `data-kb-sidecar-agent` attribute value and the `<summary>` text
/// — safe in both positions). Mirrors the repo-wide attribute-escape
/// convention (`meta_edit::escape_attr` / `memory::escape_attr`):
/// `&`/`<`/`>`/`"`, in that order so `&amp;` isn't re-escaped by a later
/// rule.
fn escape_sidecar_agent_id(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Reverse of [`escape_sidecar_agent_id`], for [`extract_sidecar_text_block`].
fn unescape_sidecar_agent_id(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Best-effort parse of the sidecar-text container back into `(agent_id,
/// text)` pairs — `agent_id` from the `data-kb-sidecar-agent` attribute,
/// `text` the HTML-unescaped `<pre>` body (which may itself carry
/// [`sidecar_truncation_marker`]'s marker line when the agent was
/// over-budget). For tests/consumers that want to inspect what got
/// embedded — NOT a resumable-transcript reconstruction (the truncation is
/// lossy by design). Returns an empty `Vec` when the container is absent OR
/// malformed — never a hard failure, mirrors [`extract_subagents_block`]'s
/// never-hard-fail contract (empty `Vec` here instead of `None` since a
/// partial parse is still useful to a caller).
pub fn extract_sidecar_text_block(html: &str) -> Vec<(String, String)> {
    let marker = format!(r#"id="{SIDECAR_TEXT_BLOCK_ID}""#);
    let Some(id_at) = html.find(&marker) else {
        return Vec::new();
    };
    let Some(section_open_end) = html[id_at..].find('>').map(|i| i + id_at + 1) else {
        return Vec::new();
    };
    let Some(section_close_rel) = html[section_open_end..].find("</section>") else {
        return Vec::new();
    };
    let body = &html[section_open_end..section_open_end + section_close_rel];

    let attr_marker = r#"data-kb-sidecar-agent=""#;
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = body[cursor..].find(attr_marker) {
        let attr_start = cursor + rel + attr_marker.len();
        let Some(attr_end_rel) = body[attr_start..].find('"') else {
            break;
        };
        let agent_id = unescape_sidecar_agent_id(&body[attr_start..attr_start + attr_end_rel]);

        let Some(pre_open_rel) = body[attr_start..].find("<pre>") else {
            break;
        };
        let pre_start = attr_start + pre_open_rel + "<pre>".len();
        let Some(pre_close_rel) = body[pre_start..].find("</pre>") else {
            break;
        };
        let pre_end = pre_start + pre_close_rel;
        out.push((agent_id, html_unescape(&body[pre_start..pre_end])));
        cursor = pre_end;
    }
    out
}

/// Remove an existing `<section id="{id}" hidden>…</section>` block (plus
/// one trailing newline when present) wherever it sits in `html`. A no-op
/// when `id` isn't present. [`strip_script_block`]'s sibling for a
/// non-`<script>` tail block, same trailing-newline-consumption idiom —
/// see [`replace_sidecar_text_block`]. Safe against a literal `</section>`
/// inside sidecar content: that content only ever lands inside an escaped
/// `<pre>` (rendered as `&lt;/section&gt;`), so the first raw `</section>`
/// after the marker is always the container's own close tag.
fn strip_section_block(html: &str, id: &str) -> String {
    let marker = format!(r#"<section id="{id}" hidden>"#);
    let Some(section_at) = html.find(&marker) else {
        return html.to_string();
    };
    // R14 — the un-hide companion <style> (see `sidecar_unhide_style_tag`)
    // sits immediately before the <section> on captures written since this
    // change; strip it too so a `--refresh-subagents` re-capture never
    // leaves an orphaned rule behind. Absent on older captures already on
    // disk — falling back to just the section start keeps those clean.
    let style = sidecar_unhide_style_tag(id);
    let start = if html[..section_at].ends_with(style.as_str()) {
        section_at - style.len()
    } else {
        section_at
    };
    let Some(end_rel) = html[start..].find("</section>") else {
        return html.to_string();
    };
    let mut tail_start = start + end_rel + "</section>".len();
    if html[tail_start..].starts_with('\n') {
        tail_start += 1;
    }
    format!("{}{}", &html[..start], &html[tail_start..])
}

/// Byte offset right after the [`SUBAGENTS_BLOCK_ID`] block's closing
/// `</script>` (and its one trailing newline, when present) — the
/// insertion point [`replace_sidecar_text_block`] prefers, keeping the
/// envelope's tail in digest-then-evidence order. `None` when the capture
/// has no subagents digest block (an old/imported capture, or a session
/// with sidecars but none the digest walk found — shouldn't happen in
/// practice since both come from the same sidecar walk, but the caller
/// falls back to right-before-`</body>` either way).
fn subagents_block_end(html: &str) -> Option<usize> {
    let marker = format!(r#"<script type="application/json" id="{SUBAGENTS_BLOCK_ID}">"#);
    let start = html.find(&marker)?;
    let close_rel = html[start..].find("</script>")?;
    let mut end = start + close_rel + "</script>".len();
    if html[end..].starts_with('\n') {
        end += 1;
    }
    Some(end)
}

/// Rewrite the sidecar-text tail block in an EXISTING capture's HTML,
/// idempotently: strip whatever block is already present (if any — makes
/// repeated refreshes idempotent) and splice `block` — the exact output of
/// [`render_sidecar_text_block`] — in right after the [`SUBAGENTS_BLOCK_ID`]
/// block when one exists, else right before `</body>` (mirrors
/// [`replace_subagents_block`]'s fallback). `block == None` removes the
/// block and re-adds nothing (mirrors "absent when empty"). Never touches
/// the `<pre>` transcript or any other tail block — the `<pre>`
/// byte-identity is invariant #11's round-trip guarantee.
pub fn replace_sidecar_text_block(html: &str, block: Option<String>) -> String {
    let stripped = strip_section_block(html, SIDECAR_TEXT_BLOCK_ID);
    let Some(block) = block else {
        return stripped;
    };
    if let Some(at) = subagents_block_end(&stripped) {
        format!("{}{}\n{}", &stripped[..at], block, &stripped[at..])
    } else {
        match stripped.rfind("</body>") {
            Some(at) => format!("{}{}\n{}", &stripped[..at], block, &stripped[at..]),
            None => format!("{stripped}{block}\n"),
        }
    }
}

// --- indexed `code` field cap (W0.6 amendment, 2026-07-22) ------------------

/// The bug this constant fixes: W0.6 (2026-07-21) stopped clearing
/// `fields.code` for `memory-session` docs (`indexer::prepare_doc`), so the
/// parser's document-wide `pre code, pre` sweep (parser.rs's `code_blocks`
/// selector) — which matches BOTH the main transcript `<pre>` AND every
/// [`SIDECAR_TEXT_BLOCK_ID`] `<pre>` — flowed into the lance `code` FTS
/// column uncapped. `code` is an Arrow `Utf8`/`StringArray` column
/// (`T::Offset = i32`); when lance interleaves many rows' `code` values into
/// one merged batch (empirically, `upsert_docs`'s `merge_insert` — see
/// `storage/lance.rs` — hits this exact path, `interleave_batches` in
/// `lance`'s `dataset::write::merge_insert`, when re-upserting rows that
/// already exist; a small table like the 916-row sessions kb fits inside a
/// SINGLE physical batch, so the whole corpus's `code` bytes land in one
/// `interleave` call), arrow-select's `interleave_bytes`
/// (`arrow-select-57.3.1/src/interleave.rs:180`) accumulates a RUNNING
/// TOTAL of bytes across every interleaved value and panics
/// (`.expect("overflow")`) once that total exceeds `i32::MAX`
/// (2,147,483,647 bytes, ~2.147 GB) — a CORPUS-WIDE cumulative ceiling, not
/// a per-row one. On the live corpus this fired on `kb reindex --kb
/// sessions` (the upsert that produced the oversized column) and then on
/// every subsequent `kb recollect` call (`ensure_fts_index` is idempotent
/// but re-scans on `recollect`'s route, and `recollect` fires on every hook
/// turn), i.e. panicking on a schedule for as long as the corpus's total
/// `code` bytes stayed over the threshold.
///
/// Sizing: this cap must keep `rows × cap` safely under the 2.147 GB
/// ceiling EVEN IF THE WHOLE CORPUS LANDS IN ONE INTERLEAVED BATCH (the
/// observed failure mode above — lance's batching is by fragment/physical
/// batch size, not a hard per-call row limit kb-core controls, so sizing
/// for "one big batch" is the only assumption that's actually safe; see
/// the lance-batching investigation in the W0.6-fix commit notes). At
/// `32 KiB` (32,768 = 2^15 bytes) per row:
/// - the ceiling (`i32::MAX` = 2,147,483,647 bytes) is only reached at
///   `2,147,483,647 / 32,768 ≈ 65,536` (2^16) session rows — today's
///   corpus (916 rows) is ~1.4% of that, i.e. ~71x headroom right now;
/// - at exactly `32,768` (2^15) rows — ~36x today's corpus, comfortably
///   past "low tens of thousands" — the worst-case sum is
///   `32,768 × 32,768 = 2^30 = 1 GiB`, exactly HALF the ceiling (2x
///   headroom); the corpus would need to DOUBLE AGAIN, to 65,536 rows
///   (~71x today), before overflow becomes possible at all, and even then
///   only if EVERY single row simultaneously sat at the cap (unrealistic —
///   most sessions are far smaller than 32 KiB of transcript+sidecar
///   text).
///
/// This is a cap on the WHOLE `code` field — main transcript AND
/// sidecar-text combined ([`truncate_code_field`]), applied post-parse in
/// `indexer::prepare_doc`'s memory-session branch. A single per-row guard
/// is simplest to reason about and is what actually bounds the
/// corpus-wide sum, regardless of how a row's bytes split between the two
/// sources (a session with a huge sidecar block and a tiny transcript is
/// exactly as safe as one the other way around). Deliberately conservative
/// over generous, matching the sidecar-text block's own framing
/// ([`SIDECAR_TEXT_TOTAL_CAP_BYTES`]): BM25 exact-token search on the
/// head+tail of a transcript still covers the overwhelming majority of
/// real debugging lookups; `code` is an occasional-lookup evidence lane,
/// not a resumable-transcript guarantee.
pub const SESSION_CODE_FIELD_CAP_BYTES: usize = 32 * 1024;

/// Fixed reserve, carved out of the byte budget BEFORE computing the
/// HEAD/TAIL split, sized so [`code_field_truncation_marker`]'s rendered
/// text can NEVER push [`truncate_code_field`]'s output past the budget —
/// the marker is fixed prose plus a decimal dropped-byte count, and even a
/// `usize::MAX`-sized drop needs under 20 digits, so 64 bytes leaves wide
/// headroom. Unlike the sidecar-text marker (whose overshoot the RAW-byte
/// budget only APPROXIMATELY bounds — see [`SIDECAR_TEXT_TOTAL_CAP_BYTES`]'s
/// doc comment), this cap's whole job is to hard-bound the corpus-wide
/// Arrow `code` column sum, so "approximately the budget" isn't good
/// enough: [`truncate_code_field`]'s output length is a proven `<=`
/// [`SESSION_CODE_FIELD_CAP_BYTES`] (see the unit tests), not just close to
/// it.
const CODE_FIELD_MARKER_RESERVE_BYTES: usize = 64;

/// The fixed marker line spliced between an over-budget `code` field's kept
/// HEAD and TAIL halves, reporting the exact number of RAW bytes dropped.
/// Deterministic text — pinned by tests. Distinct wording from
/// [`sidecar_truncation_marker`] so the two truncation sites are
/// distinguishable in a search hit.
fn code_field_truncation_marker(dropped_bytes: usize) -> String {
    format!("\n[kb-code: truncated {dropped_bytes} bytes]\n")
}

/// Cap the indexer's `code` field to [`SESSION_CODE_FIELD_CAP_BYTES`] — see
/// that constant's doc comment for the overflow this fixes and the sizing
/// arithmetic. Called from `indexer::prepare_doc`'s memory-session branch,
/// on `fields.code` post-parse.
///
/// `code` at this point is the PARSER's output (`parser::Fields::code`,
/// built from `.text()` over the `pre code, pre` selector) — already
/// HTML-UNESCAPED plain text, NOT still carrying `&amp;`/`&lt;`/`&gt;`
/// entities: html5ever decodes entities into real characters while
/// building the DOM, and `scraper::ElementRef::text()` (which
/// `collect_text_blocks` calls) reads the already-decoded text nodes.
/// `code` is also never re-serialized back into HTML — it's an
/// index-time-only Arrow/FTS column, computed fresh from the artifact on
/// every index pass and never written back to disk (invariant #27: the
/// on-disk `.html` artifact is untouched). So unlike
/// [`truncate_and_escape_agent_raw`]'s sidecar scheme — which truncates
/// RAW pre-escape bytes and THEN HTML-escapes, because that text is about
/// to be spliced into a `<pre>` block on disk — this function has no
/// entity-boundary concern and no escape step at all: it only needs to be
/// UTF-8 char-boundary-safe.
///
/// Under budget: returned byte-identical, untouched (the common case —
/// most sessions are far smaller than the cap). Over budget: keeps the
/// HEAD 60% / TAIL 40% (reusing [`sidecar_head_tail_split`]'s arithmetic),
/// each end snapped to the nearest UTF-8 char boundary via
/// [`floor_char_boundary`]/[`ceil_char_boundary`], joined by
/// [`code_field_truncation_marker`]. [`CODE_FIELD_MARKER_RESERVE_BYTES`] is
/// carved out of the budget BEFORE the head/tail split, so the marker's
/// own bytes can never push the result over the cap — see that constant's
/// doc comment for why this needs to be a hard ceiling, not an
/// approximate one.
pub fn truncate_code_field(code: &str) -> String {
    truncate_code_field_to(code, SESSION_CODE_FIELD_CAP_BYTES)
}

/// [`truncate_code_field`] parameterised on `budget` — pulled out so unit
/// tests can exercise the truncation branch with a small, human-scale
/// budget instead of [`SESSION_CODE_FIELD_CAP_BYTES`]'s real (32 KiB) size.
fn truncate_code_field_to(code: &str, budget: usize) -> String {
    if code.len() <= budget {
        return code.to_string();
    }
    let inner_budget = budget.saturating_sub(CODE_FIELD_MARKER_RESERVE_BYTES);
    let (head_len, tail_len) = sidecar_head_tail_split(inner_budget);
    let head_end = floor_char_boundary(code, head_len);
    let tail_start = ceil_char_boundary(code, code.len() - tail_len).max(head_end);
    let dropped = tail_start - head_end;
    let marker = code_field_truncation_marker(dropped);
    format!("{}{marker}{}", &code[..head_end], &code[tail_start..])
}

// --- touched-artifact extraction (S4) ---------------------------------------

/// One artifact the touches scan can match against. Built from the
/// daemon's per-kb lance index (one entry per indexed doc) at the
/// route's request boundary.
#[derive(Debug, Clone)]
pub struct KnownDoc {
    /// The 12-hex `ArtifactId::from_path` stem.
    pub id: String,
    /// Source-root-relative path. The path-substring match is the
    /// "fuzzy" lane: a transcript that mentions `notes/foo.html` will
    /// pull in `<kb>/notes/foo.html` even when the transcript didn't
    /// carry the literal id.
    pub source_relative: String,
}

/// Confidence of a touches scan. Pure-exact when every matched id
/// came from a literal 12-hex hit; `Fuzzy` flips on the first
/// path-substring match (those are weaker signals — a transcript
/// could mention `index.html` without intending the artifact).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TouchesConfidence {
    Exact,
    Fuzzy,
}

/// CT-A6 — one touched artifact plus the join-tier that found it. Additive
/// alongside `TouchesResponse`'s aggregate `confidence`: the aggregate
/// answers "should I trust this whole scan", this answers "which of these
/// rows specifically". Both tiers are honest signals, not a reliability
/// ranking — `Fuzzy` catches discussed-not-opened artifacts the exact join
/// (`session_files.target_artifact_id`) misses.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct TouchedArtifact {
    pub id: String,
    pub confidence: TouchesConfidence,
}

/// Pure: scan a transcript body for artifact references and return the
/// deduped set of ids it touched (each tagged with its own join-tier), plus
/// an aggregate `confidence` flag indicating whether every match was a
/// literal id hit (Exact) or whether any came from a source-relative path
/// substring (Fuzzy).
///
/// The matching is intentionally cheap (substring scans, not a parser):
/// - **literal 12-hex ids** → Exact lane. Mirrors the
///   `ArtifactId::from_path` short-prefix form used in SPA permalinks
///   (`/a/kb/<id>`), CLI verbs (`kb cat <id>`), and anywhere the
///   agent writes an id verbatim.
/// - **source-relative path substrings** → Fuzzy lane. Used when the
///   transcript quotes a filename or relative path the agent worked
///   on (e.g., `notes/2026-05.html`).
///
/// `known` is the index of candidate artifacts; the function never
/// invents an id. Linear-scan; corpora are small enough that a smarter
/// index isn't justified for v0.14.
pub fn extract_touched_ids(
    transcript_text: &str,
    known: &[KnownDoc],
) -> (Vec<TouchedArtifact>, TouchesConfidence) {
    if known.is_empty() {
        return (Vec::new(), TouchesConfidence::Exact);
    }
    let mut hits: std::collections::BTreeMap<String, TouchesConfidence> =
        std::collections::BTreeMap::new();
    let mut fuzzy = false;
    for doc in known {
        if !doc.id.is_empty() && transcript_text.contains(&doc.id) {
            hits.insert(doc.id.clone(), TouchesConfidence::Exact);
            continue;
        }
        if !doc.source_relative.is_empty() && transcript_text.contains(&doc.source_relative) {
            // Exact wins if some other `known` entry already matched this
            // same id by literal hit; never downgrade a row already proven.
            hits.entry(doc.id.clone())
                .or_insert(TouchesConfidence::Fuzzy);
            fuzzy = true;
        }
    }
    let confidence = if fuzzy {
        TouchesConfidence::Fuzzy
    } else {
        TouchesConfidence::Exact
    };
    let artifacts = hits
        .into_iter()
        .map(|(id, confidence)| TouchedArtifact { id, confidence })
        .collect();
    (artifacts, confidence)
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        // Trim trailing whitespace introduced by a mid-token cut so
        // the preview ends cleanly.
        while out.ends_with(char::is_whitespace) {
            out.pop();
        }
    }
    out
}

// --- narrative threads (P7) -------------------------------------------------

/// Sessions in the same folder within this many seconds of each other are one
/// continued effort — a "thread". ~2 days, so an overnight or weekend gap
/// doesn't split a multi-day push, but a fortnight later starts a new thread.
pub const THREAD_GAP_SECS: i64 = 86_400 * 2;

/// Pure: given session `(started_at, ended_at)` spans SORTED ASC by start,
/// return the indices at which a NEW thread begins — a session whose start is
/// more than `gap_secs` after the running maximum end of the current thread.
/// Index 0 is implicitly the first thread's start (never returned). The caller
/// groups by folder first, then splits each group with this. Deterministic.
pub fn thread_boundaries(spans: &[(i64, i64)], gap_secs: i64) -> Vec<usize> {
    let mut bounds = Vec::new();
    let mut last_end = i64::MIN;
    for (i, &(start, end)) in spans.iter().enumerate() {
        if i > 0 && start.saturating_sub(last_end) > gap_secs {
            bounds.push(i);
        }
        last_end = last_end.max(end);
    }
    bounds
}

// --- corpus-mount resolution (S3) -------------------------------------------

/// One mounted corpus: a kb name and its source root on disk. The enrich
/// hook resolves a session's touched-file paths against the full set of
/// mounts so a session captured in `[kb.sessions]` can link to the artifact
/// it edited in `[kb.research]` (the central cross-corpus link, A7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusMount {
    pub kb: String,
    /// Source root on disk. Matched via [`crate::paths::doc_rel_path`], which
    /// canonicalises both sides (symlinked roots resolve, invariant #27).
    pub source_root: PathBuf,
}

/// Process-global mount table. The daemon (kb-server) sets this once at
/// startup from config (and again on a config-reload restart) via
/// [`set_corpus_mounts`]; the indexer's session-capture hook reads it through
/// [`corpus_mounts`]. It is daemon-global static configuration — not
/// per-index state — so it lives here rather than threaded through every
/// indexer signature. Empty by default (tests + a fresh daemon resolve
/// nothing → `in_corpus = 0`, which is the correct out-of-corpus answer).
static CORPUS_MOUNTS: RwLock<Option<Arc<Vec<CorpusMount>>>> = RwLock::new(None);

/// Install the daemon's corpus mount table. Sorts most-specific-root-first so
/// a nested mount (`/srv/a/b` under `/srv/a`) attributes to the deeper kb.
pub fn set_corpus_mounts(mut mounts: Vec<CorpusMount>) {
    mounts.sort_by(|a, b| {
        b.source_root
            .as_os_str()
            .len()
            .cmp(&a.source_root.as_os_str().len())
    });
    *CORPUS_MOUNTS.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(mounts));
}

/// Serializes tests that mutate the process-global mount table (across this
/// module and `session_render`), so parallel execution can't interleave one
/// test's `set_corpus_mounts` with another's read. Poison-tolerant.
#[cfg(test)]
pub(crate) static MOUNT_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The current mount table (cheap `Arc` clone). Empty when unset.
pub fn corpus_mounts() -> Arc<Vec<CorpusMount>> {
    CORPUS_MOUNTS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// Resolve one transcript file path to its corpus membership: `(in_corpus,
/// target_kb, target_artifact_id)`. Pure given `(path, cwd, mounts)`.
///
/// `path` is verbatim from the transcript — absolute, or relative to the
/// session's working dir. Relative paths are absolutised against `cwd` (the
/// session's modal cwd); a relative path with no `cwd` can't be resolved.
/// Matching reuses [`crate::paths::doc_rel_path`] (canonicalises both sides,
/// handles symlinked roots, returns empty when the path isn't under the
/// root), so the artifact id is `ArtifactId::from_path(rel)` — the same
/// source-relative id the indexer assigned (invariant #27), with no lance
/// lookup.
pub fn resolve_corpus_path(
    path: &str,
    cwd: Option<&str>,
    mounts: &[CorpusMount],
) -> (bool, Option<String>, Option<String>) {
    if mounts.is_empty() {
        return (false, None, None);
    }
    let p = Path::new(path);
    let abs: PathBuf = if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(c) = cwd.filter(|c| !c.is_empty()) {
        Path::new(c).join(p)
    } else {
        return (false, None, None);
    };
    let abs_str = abs.to_string_lossy();
    for m in mounts {
        let rel = crate::paths::doc_rel_path(&abs_str, &m.source_root);
        if !rel.is_empty() {
            let id = crate::ids::ArtifactId::from_path(&rel).as_str().to_string();
            return (true, Some(m.kb.clone()), Some(id));
        }
    }
    (false, None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapper(sid: &str, jsonl: &str) -> String {
        let esc = jsonl
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        format!(
            r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Session transcript 20260524T100000Z</title>
<meta name="kb-category" content="memory-session">
<meta name="kb-decay" content="fast">
<meta name="kb-session" content="{sid}">
</head><body>
<h1>Session transcript 20260524T100000Z</h1>
<pre>{esc}</pre>
</body></html>
"#
        )
    }

    #[test]
    fn session_digest_assembles_high_signal_fields() {
        let parse = SessionParse {
            facts: SessionFacts {
                session_id: "s1".into(),
                started_at: 0,
                message_count: 3,
                first_user_prompt: Some("why is the parser flaky".into()),
            },
            activity: SessionActivity {
                ai_title: Some("Fix the flaky parser".into()),
                cwd: Some("/home/u/proj/kb".into()),
                git_branch: Some("main".into()),
                decisions: vec![Decision {
                    kind: "question".into(),
                    prompt: "Which fix approach?".into(),
                    answer: Some("root cause".into()),
                }],
                commits: vec![Commit {
                    kind: "commit".into(),
                    sha: Some("abc1234".into()),
                    subject: Some("fix(parser): handle empty <pre>".into()),
                }],
                files: vec![FileTouch {
                    path: "/home/u/proj/kb/src/parser.rs".into(),
                    action: FileAction::Edit,
                }],
                edited_paths: vec!["/home/u/proj/kb/src/parser.rs".into()],
                last_assistant_text: Some("Fixed the root cause; tests green.".into()),
                ..Default::default()
            },
        };
        let dig = session_digest(&parse);
        assert!(dig.contains("Fix the flaky parser"), "{dig}");
        assert!(dig.contains("why is the parser flaky"), "{dig}");
        assert!(
            dig.contains("decision: Which fix approach? -> root cause"),
            "{dig}"
        );
        assert!(dig.contains("commits: fix(parser): handle empty"), "{dig}");
        assert!(dig.contains("files: parser.rs"), "{dig}");
        // R3/D1-B — the closure line.
        assert!(
            dig.contains("closed: Fixed the root cause; tests green."),
            "{dig}"
        );
        assert!(dig.contains("project: kb"), "{dig}");
        assert!(dig.contains("branch: main"), "{dig}");
        // The basename is deduped across edited_paths + files.
        assert_eq!(dig.matches("parser.rs").count(), 1, "{dig}");
        // D1-B — the excerpt is title · first-prompt · closed, composed
        // independently of the digest body's own line order (which puts
        // decisions/commits/files BEFORE the closure) so closure always
        // shows on a search/recollect card regardless of digest length.
        let exc = session_digest_excerpt(&parse);
        assert_eq!(
            exc,
            "Fix the flaky parser · why is the parser flaky · closed: Fixed the root cause; tests green."
        );
        assert!(exc.chars().count() <= 400);
    }

    // W4/R8/ADD-2 — the digest-unchanged proof: a `grok_job` research row
    // must NOT appear in `researched:`, and adding one to an otherwise
    // identical `SessionActivity` must produce a BYTE-IDENTICAL digest to
    // the same activity without it (the reindex budget is spent — this row
    // is queryable via `session_research`/`by-job`, never via the index).
    #[test]
    fn session_digest_excludes_grok_job_research_rows() {
        let base = SessionParse {
            facts: SessionFacts {
                session_id: "s1".into(),
                started_at: 0,
                message_count: 3,
                first_user_prompt: Some("audit the queue depth".into()),
            },
            activity: SessionActivity {
                ai_title: Some("Queue depth audit".into()),
                research: vec![Research {
                    kind: "kb_search".into(),
                    query: "queue backlog".into(),
                }],
                ..Default::default()
            },
        };
        let mut with_grok = base.clone();
        with_grok.activity.research.push(Research {
            kind: "grok_job".into(),
            query: "01KY9XHCWKWBJSFDHW70G56HYY".into(),
        });

        let dig_base = session_digest(&base);
        let dig_with_grok = session_digest(&with_grok);
        assert_eq!(
            dig_base, dig_with_grok,
            "a grok_job row must not change the digest body"
        );
        assert!(dig_with_grok.contains("researched: queue backlog"));
        assert!(
            !dig_with_grok.contains("01KY9XHCWKWBJSFDHW70G56HYY"),
            "{dig_with_grok}"
        );

        let exc_base = session_digest_excerpt(&base);
        let exc_with_grok = session_digest_excerpt(&with_grok);
        assert_eq!(
            exc_base, exc_with_grok,
            "a grok_job row must not change the digest excerpt either"
        );
    }

    // invariant:11 digest-not-jsonl
    #[test]
    fn session_digest_is_not_raw_jsonl() {
        let jsonl = concat!(
            r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"investigate the qwortzle subsystem"},"cwd":"/home/u/proj/kb","gitBranch":"main"}"#,
            "\n",
            r#"{"aiTitle":"Qwortzle investigation"}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"claude","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/home/u/proj/kb/src/qwortzle.rs"}}]}}"#,
            "\n",
        );
        let html = wrapper("s1", jsonl);
        let parse = parse_session_html_full(&html, "session-20260524T100000Z-s1.html", 0);
        let dig = session_digest(&parse);
        assert!(dig.contains("Qwortzle investigation"), "{dig}");
        assert!(dig.contains("investigate the qwortzle subsystem"), "{dig}");
        assert!(dig.contains("qwortzle.rs"), "{dig}");
        // None of the raw-JSON scaffolding leaks into the index surface.
        for noise in [
            "tool_use",
            "promptSource",
            "\"role\"",
            "aiTitle",
            "file_path",
        ] {
            assert!(
                !dig.contains(noise),
                "digest must not carry raw json {noise:?}: {dig}"
            );
        }
    }

    #[test]
    fn recollect_order_lets_a_stronger_match_beat_a_committing_session() {
        // The documented #1 failure mode to avoid: a high-error, no-commit
        // EXPLORATORY session with a stronger digest match (rank 0) must
        // outrank a clean, tangentially-COMMITTING session (rank 3) for a
        // "why / has this been done" query. Success is a tie-break, not a
        // score term, so relevance wins.
        let now = 1_800_000_000;
        let same_start = now - 10 * 86_400; // both 10 days old → equal recency
        let mut c = vec![
            RecollectCandidate {
                session_id: "committed".into(),
                rank: 3,
                started_at: same_start,
                error_count: 0,
            },
            RecollectCandidate {
                session_id: "exploratory".into(),
                rank: 0,
                started_at: same_start,
                error_count: 5,
            },
        ];
        recollect_order(&mut c, now);
        assert_eq!(
            c[0].session_id, "exploratory",
            "stronger digest match must rank first despite errors + no commit"
        );

        // At EQUAL rank + recency, the cheap success tie-break (fewer errors)
        // decides — deterministically.
        let mut tie = vec![
            RecollectCandidate {
                session_id: "messy".into(),
                rank: 1,
                started_at: same_start,
                error_count: 9,
            },
            RecollectCandidate {
                session_id: "clean".into(),
                rank: 1,
                started_at: same_start,
                error_count: 0,
            },
        ];
        recollect_order(&mut tie, now);
        assert_eq!(tie[0].session_id, "clean", "tie-break prefers fewer errors");
    }

    #[test]
    fn recollect_recency_is_gentle_not_dominant() {
        // A 1-year-old session keeps ~half its weight (365-day half-life), so
        // old work still surfaces for "has this been done?".
        let now = 1_800_000_000;
        let fresh = recollect_score(0, now, now);
        let year_old = recollect_score(0, now - 365 * 86_400, now);
        let ratio = year_old / fresh;
        assert!(
            (0.45..=0.55).contains(&ratio),
            "1yr-old keeps ~half weight, got {ratio}"
        );
    }

    #[test]
    fn parse_extracts_research_signals() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"kb search \"flux capacitor\" --kb x"}}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"WebSearch","input":{"query":"rust async traits"}}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t3","name":"Task","input":{"description":"explore the codebase","subagent_type":"Explore"}}]}}"#,
            "\n",
            // GC-B8 — newer transcripts name the same delegation tool "Agent"
            // (the subagent-iceberg defect: this arm went undetected because
            // every hand-written test only ever used "Task").
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t5","name":"Agent","input":{"description":"audit the config loader","subagent_type":"Explore"}}]}}"#,
            "\n",
            r##"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t4","name":"ExitPlanMode","input":{"plan":"# Plan\nstep one\nstep two"}}]}}"##,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        let kinds: Vec<&str> = act.research.iter().map(|r| r.kind.as_str()).collect();
        assert!(kinds.contains(&"kb_search"), "{:?}", act.research);
        assert!(kinds.contains(&"web"), "{:?}", act.research);
        assert!(kinds.contains(&"subagent"), "{:?}", act.research);
        assert!(kinds.contains(&"plan_span"), "{:?}", act.research);
        let kb = act.research.iter().find(|r| r.kind == "kb_search").unwrap();
        assert_eq!(kb.query, "search flux capacitor");
        let web = act.research.iter().find(|r| r.kind == "web").unwrap();
        assert_eq!(web.query, "rust async traits");
        let plan = act.research.iter().find(|r| r.kind == "plan_span").unwrap();
        assert_eq!(plan.query, "# Plan");
        // Both the "Task" and "Agent" tool names surface as "subagent" —
        // neither shadows the other.
        let subagents: Vec<&str> = act
            .research
            .iter()
            .filter(|r| r.kind == "subagent")
            .map(|r| r.query.as_str())
            .collect();
        assert_eq!(
            subagents,
            vec!["explore the codebase", "audit the config loader"],
            "{:?}",
            act.research
        );
    }

    #[test]
    fn parse_extracts_grok_job_from_paired_tool_result() {
        // W4/R8/ADD-2 — a Bash tool_use invoking grokclaude, paired to a
        // tool_result whose human-format output carries the `job:    <ulid>`
        // line (`grokclaude::lib::human_format`, verified live).
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"g1","name":"Bash","input":{"command":"grokclaude research run --goal 'audit the queue'"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"g1","content":"status: done\njob:    01KY9XHCWKWBJSFDHW70G56HYY\nreport: .grokclaude/jobs/01KY9XHCWKWBJSFDHW70G56HYY/report.md\n"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        let hits: Vec<&Research> = act
            .research
            .iter()
            .filter(|r| r.kind == "grok_job")
            .collect();
        assert_eq!(hits.len(), 1, "{:?}", act.research);
        assert_eq!(hits[0].query, "01KY9XHCWKWBJSFDHW70G56HYY");
    }

    #[test]
    fn parse_extracts_grok_job_falls_back_to_command_when_no_ulid() {
        // No ulid recoverable from the result (e.g. an error before the job
        // id was assigned) — conservative fallback to the command line,
        // never a fabricated id.
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"g2","name":"Bash","input":{"command":"grokclaude session round --job unknown"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"g2","content":"error: no such job"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        let hits: Vec<&Research> = act
            .research
            .iter()
            .filter(|r| r.kind == "grok_job")
            .collect();
        assert_eq!(hits.len(), 1, "{:?}", act.research);
        assert_eq!(hits[0].query, "grokclaude session round --job unknown");
    }

    #[test]
    fn parse_extracts_grok_job_falls_back_when_result_has_ambiguous_ulids() {
        // Two DISTINCT ulid-shaped tokens in the result — not confidently
        // ONE job id, so fall back rather than guess.
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"g3","name":"Bash","input":{"command":"grokclaude fleet status"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"g3","content":"01KY9XHCWKWBJSFDHW70G56HYY 01KY7RB2CBNVC7SB7Z9XR4Y9D1"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        let hits: Vec<&Research> = act
            .research
            .iter()
            .filter(|r| r.kind == "grok_job")
            .collect();
        assert_eq!(hits.len(), 1, "{:?}", act.research);
        assert_eq!(hits[0].query, "grokclaude fleet status");
    }

    #[test]
    fn grok_job_ignores_non_grokclaude_bash_calls() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"g4","name":"Bash","input":{"command":"echo 01KY9XHCWKWBJSFDHW70G56HYY"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"g4","content":"01KY9XHCWKWBJSFDHW70G56HYY"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert!(
            act.research.iter().all(|r| r.kind != "grok_job"),
            "{:?}",
            act.research
        );
    }

    #[test]
    fn parse_extracts_grok_job_child_side_from_adapter_meta_job_ulid() {
        // W5/R8/ADD-2 — the CHILD side of the join: a grok session capture's
        // OWN adapter-meta line (as `kb-capture-grok.sh` stamps it) carries
        // `job_ulid`, and that alone must yield the same `grok_job` research
        // row shape the driver side emits, with the ulid verbatim as the
        // query (never derived/guessed).
        let jsonl = concat!(
            r#"{"sessionId":"019f93d8-b58c-7b82-9fc6-bffef4dd7961","type":"adapter-meta","adapter":"kb-capture-grok/1","harness":"grok","driver":"grokclaude","job_ulid":"01KY9XHCWKWBJSFDHW70G56HYY","job_type":"build","round":"0"}"#,
            "\n",
            r#"{"sessionId":"019f93d8-b58c-7b82-9fc6-bffef4dd7961","type":"user","message":{"role":"user","content":[{"type":"text","text":"add a status filter"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.harness.as_deref(), Some("grok"));
        let hits: Vec<&Research> = act
            .research
            .iter()
            .filter(|r| r.kind == "grok_job")
            .collect();
        assert_eq!(hits.len(), 1, "{:?}", act.research);
        assert_eq!(hits[0].query, "01KY9XHCWKWBJSFDHW70G56HYY");
    }

    #[test]
    fn parse_grok_adapter_meta_without_job_ulid_emits_no_research_row() {
        // A direct (non-grokclaude) `--session-dir` capture has no job to
        // join against — no `job_ulid` field at all — and must not
        // fabricate a `grok_job` row.
        let jsonl = concat!(
            r#"{"sessionId":"019f93d8-0000-0000-0000-000000000000","type":"adapter-meta","adapter":"kb-capture-grok/1","harness":"grok"}"#,
            "\n",
            r#"{"sessionId":"019f93d8-0000-0000-0000-000000000000","type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert!(act.research.iter().all(|r| r.kind != "grok_job"));
    }

    #[test]
    fn parse_extracts_grok_job_child_side_deduplicates_repeat_ulid() {
        // A resumed grok session (`grok -r <uuid>`) re-captured mid-run
        // still stamps `job_ulid` on every adapter-meta occurrence (there's
        // only ever one, the first line) — this just pins that a second
        // capture pass over the SAME jsonl never double-inserts the row.
        let jsonl = concat!(
            r#"{"sessionId":"019f93d8-b58c-7b82-9fc6-bffef4dd7961","type":"adapter-meta","adapter":"kb-capture-grok/1","harness":"grok","job_ulid":"01KY9XHCWKWBJSFDHW70G56HYY"}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        let hits: Vec<&Research> = act
            .research
            .iter()
            .filter(|r| r.kind == "grok_job")
            .collect();
        assert_eq!(hits.len(), 1, "{:?}", act.research);
    }

    // --- W5/R10 — task_output_paths -----------------------------------

    #[test]
    fn task_output_paths_extracts_the_bracketed_tmp_shape() {
        let text = "[Truncated. Full output: /tmp/claude-1000/-home-user-project-kb/82a94f49-a658-4770-93cc-1ef3e2d8385c/tasks/wl75we7oz.output]\n\nmore text";
        let got = task_output_paths(text);
        assert_eq!(
            got,
            vec![
                "/tmp/claude-1000/-home-user-project-kb/82a94f49-a658-4770-93cc-1ef3e2d8385c/tasks/wl75we7oz.output"
                    .to_string()
            ]
        );
    }

    #[test]
    fn task_output_paths_extracts_the_var_tmp_shape_without_brackets() {
        let text = "Full output: /var/tmp/claude-1000/proj/sid/tasks/abc123.output\nrest";
        let got = task_output_paths(text);
        assert_eq!(
            got,
            vec!["/var/tmp/claude-1000/proj/sid/tasks/abc123.output".to_string()]
        );
    }

    #[test]
    fn task_output_paths_dedupes_and_preserves_first_seen_order() {
        let text = "Full output: /tmp/x/tasks/a.output]\nblah\nFull output: /tmp/x/tasks/b.output]\nFull output: /tmp/x/tasks/a.output]\n";
        let got = task_output_paths(text);
        assert_eq!(
            got,
            vec![
                "/tmp/x/tasks/a.output".to_string(),
                "/tmp/x/tasks/b.output".to_string()
            ]
        );
    }

    #[test]
    fn task_output_paths_ignores_the_unrelated_persisted_output_marker() {
        // The generic large-tool-result mechanism ("<persisted-output>...
        // Full output saved to: .../tool-results/<id>.txt") is a DIFFERENT
        // marker shape and must never match — only the .../tasks/<id>.output
        // shape is in scope.
        let text = "<persisted-output>\nOutput too large (55.5KB). Full output saved to: /home/user/.claude/projects/-home-user-project-x/sid/tool-results/b0mlzqdi3.txt\n</persisted-output>";
        assert!(task_output_paths(text).is_empty());
    }

    #[test]
    fn task_output_paths_empty_text_yields_empty_vec() {
        assert!(task_output_paths("").is_empty());
        assert!(task_output_paths("no markers here at all").is_empty());
    }

    #[test]
    fn kb_cli_query_handles_env_prefix_and_flags() {
        assert_eq!(
            kb_cli_query("kb search \"foo bar\""),
            Some(("kb_search", "search foo bar".into()))
        );
        assert_eq!(
            kb_cli_query("cd /x && kb find widget"),
            Some(("kb_search", "find widget".into()))
        );
        assert_eq!(
            kb_cli_query("KB_DAEMON_URL=x kb related abc123"),
            Some(("kb_search", "related abc123".into()))
        );
        assert_eq!(kb_cli_query("git status"), None);
        assert_eq!(kb_cli_query("kb daemon"), None);
        // RA2 — self-referential memory verbs are NOT research (the reflect
        // loop runs these; counting them would poison its own input).
        assert_eq!(kb_cli_query("cd /x && kb recall wombat"), None);
        assert_eq!(kb_cli_query("env FOO=1 kb recollect 'flux'"), None);
        assert_eq!(kb_cli_query("kb remember \"a fact\" --salience 0.8"), None);
        assert_eq!(kb_cli_query("kb why src/foo.rs"), None);
    }

    // CT-A5 — the real kb-cli artifact-dump verbs (`cat`/`get`) are a
    // SEPARATE `artifact_open` kind, distinct from the `kb_search` family.
    // `open` is NOT a real kb-cli verb (`kb read` is the browser-open one)
    // and must NOT match.
    #[test]
    fn kb_cli_query_maps_real_artifact_dump_verbs_to_artifact_open() {
        assert_eq!(
            kb_cli_query("kb cat 9f8b7182d433"),
            Some(("artifact_open", "cat 9f8b7182d433".into()))
        );
        assert_eq!(
            kb_cli_query("cd /x && kb get abc123 --format md"),
            Some(("artifact_open", "get abc123".into()))
        );
        assert_eq!(kb_cli_query("kb open abc123"), None);
    }

    #[test]
    fn classify_research_filters_self_referential_and_playwright() {
        // RA2 — a kb recall / remember Bash call is NOT research.
        assert_eq!(
            classify_research(
                "Bash",
                Some(&serde_json::json!({"command": "kb recall \"prior work\""}))
            ),
            None
        );
        // A real kb search IS research.
        assert!(classify_research(
            "Bash",
            Some(&serde_json::json!({"command": "kb search \"prior art\""}))
        )
        .is_some());
        // Playwright/browser MCP automation is UI scaffolding, not research.
        assert_eq!(
            classify_research(
                "mcp__plugin_playwright_playwright__browser_click",
                Some(&serde_json::json!({})),
            ),
            None
        );
        // A non-UI MCP tool still surfaces as a skill signal.
        assert!(
            classify_research("mcp__some_server__doc_lookup", Some(&serde_json::json!({})))
                .is_some()
        );
    }

    // CT-A5 — funnel honesty: `kb cat`/`kb get` (real artifact-dump verbs)
    // classify as `artifact_open`, not the generic `kb_search` kind.
    #[test]
    fn classify_research_maps_cat_and_get_to_artifact_open() {
        let cat = classify_research(
            "Bash",
            Some(&serde_json::json!({"command": "kb cat 9f8b7182d433"})),
        )
        .expect("kb cat is a research signal");
        assert_eq!(cat.kind, "artifact_open");
        assert_eq!(cat.query, "cat 9f8b7182d433");

        let get = classify_research(
            "Bash",
            Some(&serde_json::json!({"command": "kb get abc123 --format md"})),
        )
        .expect("kb get is a research signal");
        assert_eq!(get.kind, "artifact_open");
        assert_eq!(get.query, "get abc123");

        // `open` is not a real kb-cli verb — must not classify at all.
        assert_eq!(
            classify_research(
                "Bash",
                Some(&serde_json::json!({"command": "kb open abc123"}))
            ),
            None
        );
    }

    #[test]
    fn parse_session_html_prefers_meta_session_over_filename() {
        let html = wrapper("from-meta", "");
        let f = parse_session_html(
            &html,
            "session-20260524T100000Z-from-filename.html",
            1_700_000_000,
        );
        assert_eq!(f.session_id, "from-meta");
    }

    // invariant:11 canonical-id
    #[test]
    fn parse_session_html_prefers_jsonl_session_id_over_truncated_meta() {
        // The capture hook truncated the meta to 24 chars; the transcript's own
        // `sessionId` is the full ground truth and must win — so `claude -r`
        // gets a resumable id and the session↔memories link aligns.
        let jsonl = r#"{"type":"user","sessionId":"7dae9ec8-3d13-4ef1-8128-41354fd20c6f","message":{"role":"user","content":"hi"}}"#;
        let html = wrapper("7dae9ec8-3d13-4ef1-8128-", jsonl);
        let f = parse_session_html(
            &html,
            "session-20260524T100000Z-7dae9ec8-3d13-4ef1-8128-.html",
            1_700_000_000,
        );
        assert_eq!(f.session_id, "7dae9ec8-3d13-4ef1-8128-41354fd20c6f");
    }

    #[test]
    fn parse_session_html_falls_back_to_filename_session_id() {
        let html = "<html><body><pre></pre></body></html>";
        let f = parse_session_html(
            html,
            "session-20260524T100000Z-from-filename.html",
            1_700_000_000,
        );
        assert_eq!(f.session_id, "from-filename");
    }

    #[test]
    fn parse_session_html_filename_stem_last_resort() {
        let html = "<html><body></body></html>";
        let f = parse_session_html(html, "random.html", 1_700_000_000);
        assert_eq!(f.session_id, "random");
    }

    #[test]
    fn started_at_parses_from_filename_when_well_formed() {
        let html = "<html></html>";
        let f = parse_session_html(html, "session-20260524T100000Z-abc.html", 0);
        // 2026-05-24 10:00:00 UTC == 1_779_616_800 unix (verify via
        // `date -u -d @1779616800` → "Sun May 24 10:00:00 UTC 2026").
        assert_eq!(f.started_at, 1_779_616_800);
    }

    #[test]
    fn started_at_falls_back_to_mtime() {
        let html = "<html></html>";
        let f = parse_session_html(html, "random.html", 42);
        assert_eq!(f.started_at, 42);
    }

    #[test]
    fn message_count_zero_when_no_pre() {
        let html = "<html><body><p>nope</p></body></html>";
        let f = parse_session_html(html, "x.html", 0);
        assert_eq!(f.message_count, 0);
        assert!(f.first_user_prompt.is_none());
    }

    #[test]
    fn message_count_counts_nonempty_jsonl_lines() {
        let jsonl = "{\"type\":\"a\"}\n{\"type\":\"b\"}\n\n{\"type\":\"c\"}\n";
        let html = wrapper("sid", jsonl);
        let f = parse_session_html(&html, "x.html", 0);
        assert_eq!(f.message_count, 3);
    }

    #[test]
    fn first_user_prompt_skips_meta_and_command_caveats() {
        let jsonl = concat!(
            "{\"type\":\"file-history-snapshot\"}\n",
            // Meta = true → skipped.
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-caveat>noise</local-command-caveat>\"},\"isMeta\":true}\n",
            // /clear command synthesis → skipped on the literal prefix.
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<command-name>/clear</command-name>\"}}\n",
            // The real user prompt — string content variant.
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello there\"}}\n",
        );
        let html = wrapper("sid", jsonl);
        let f = parse_session_html(&html, "x.html", 0);
        assert_eq!(f.first_user_prompt.as_deref(), Some("hello there"));
    }

    #[test]
    fn first_user_prompt_handles_content_block_array() {
        let jsonl = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"block prompt\"}]}}\n";
        let html = wrapper("sid", jsonl);
        let f = parse_session_html(&html, "x.html", 0);
        assert_eq!(f.first_user_prompt.as_deref(), Some("block prompt"));
    }

    #[test]
    fn first_user_prompt_truncates_to_cap() {
        let long = "x".repeat(500);
        let jsonl = format!(
            "{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"{long}\"}}}}\n"
        );
        let html = wrapper("sid", &jsonl);
        let f = parse_session_html(&html, "x.html", 0);
        assert_eq!(
            f.first_user_prompt.as_deref().map(str::len),
            Some(PROMPT_PREVIEW_MAX_CHARS)
        );
    }

    // (parse_compact_utc round-trip/malformed coverage lives with the
    // shared parser in timeparse.rs.)

    #[test]
    fn html_unescape_reverses_the_kb_capture_sed_chain() {
        assert_eq!(html_unescape("&lt;a &amp; b&gt;"), "<a & b>");
    }

    /// Entity-order pin: historical chained replace was `&lt;` then `&gt;`
    /// then `&amp;`, so `&amp;lt;` becomes the two chars `&lt;` (not `<`),
    /// and `&amp;amp;` becomes `&amp;`. Single-pass must match exactly.
    #[test]
    fn html_unescape_preserves_amp_then_entity_order() {
        assert_eq!(html_unescape("&amp;lt;"), "&lt;");
        assert_eq!(html_unescape("&amp;gt;"), "&gt;");
        assert_eq!(html_unescape("&amp;amp;"), "&amp;");
        // `&amp;gt;` decodes to the LITERAL `&gt;` (one pass, no re-decode) —
        // the inverse of the capture sed chain, same as the old replace chain.
        assert_eq!(html_unescape("&lt;&amp;gt;"), "<&gt;");
        assert_eq!(
            html_unescape("a &amp;lt; b &amp;amp; c &gt; d"),
            "a &lt; b &amp; c > d"
        );
    }

    #[test]
    fn escape_sidecar_text_is_inverse_of_html_unescape_for_three_entities() {
        let cases = [
            "",
            "plain",
            "a < b > c & d",
            "&lt;already&gt;",
            "mixed <& tags>",
            "unicode — café 日本語",
        ];
        for s in cases {
            assert_eq!(
                html_unescape(&escape_sidecar_text(s)),
                s,
                "round-trip {s:?}"
            );
        }
        // Escape direction: amp first (so a literal & never becomes a half-entity).
        assert_eq!(escape_sidecar_text("a < b & c >"), "a &lt; b &amp; c &gt;");
        assert_eq!(escape_sidecar_text("&lt;"), "&amp;lt;");
    }

    #[test]
    fn parse_session_activity_counts_skipped_unparseable_lines() {
        let jsonl = concat!(
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            "\n",
            "not-json-at-all\n",
            "{also broken\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.skipped_lines, 2);
        assert!(act.message_count >= 2);
    }

    // --- extract_touched_ids -------------------------------------------------

    fn doc(id: &str, rel: &str) -> KnownDoc {
        KnownDoc {
            id: id.to_string(),
            source_relative: rel.to_string(),
        }
    }

    /// Test-only projection: the id list, ignoring per-row confidence
    /// (most of these tests only care which ids were found).
    fn ids_of(artifacts: &[TouchedArtifact]) -> Vec<&str> {
        artifacts.iter().map(|a| a.id.as_str()).collect()
    }

    #[test]
    fn touches_returns_empty_exact_for_no_candidates() {
        let (artifacts, conf) = extract_touched_ids("anything goes", &[]);
        assert!(artifacts.is_empty());
        assert_eq!(conf, TouchesConfidence::Exact);
    }

    #[test]
    fn touches_exact_picks_literal_id_hits() {
        let known = vec![
            doc("abc123def456", "notes/a.html"),
            doc("000000000000", "other.html"),
        ];
        let transcript = "Read /a/work/abc123def456 — useful";
        let (artifacts, conf) = extract_touched_ids(transcript, &known);
        assert_eq!(ids_of(&artifacts), vec!["abc123def456"]);
        assert_eq!(artifacts[0].confidence, TouchesConfidence::Exact);
        assert_eq!(conf, TouchesConfidence::Exact);
    }

    #[test]
    fn touches_fuzzy_when_only_path_substring_matches() {
        let known = vec![doc("abc123def456", "notes/2026-05.html")];
        let transcript = "I'm reading notes/2026-05.html for context";
        let (artifacts, conf) = extract_touched_ids(transcript, &known);
        assert_eq!(ids_of(&artifacts), vec!["abc123def456"]);
        assert_eq!(artifacts[0].confidence, TouchesConfidence::Fuzzy);
        assert_eq!(conf, TouchesConfidence::Fuzzy);
    }

    #[test]
    fn touches_dedups_when_both_id_and_path_match_same_doc() {
        let known = vec![doc("abc123def456", "notes/a.html")];
        let transcript = "see /a/work/abc123def456 and notes/a.html";
        let (artifacts, _) = extract_touched_ids(transcript, &known);
        // Single hit even though both id + path reference the same doc.
        assert_eq!(ids_of(&artifacts), vec!["abc123def456"]);
    }

    #[test]
    fn touches_skips_empty_id_or_path_entries() {
        // Defensive: known list with empty strings shouldn't match
        // every transcript (a `transcript.contains("")` is always true).
        let known = vec![doc("", ""), doc("abc123def456", "")];
        let transcript = "abc123def456 is mentioned here";
        let (artifacts, _) = extract_touched_ids(transcript, &known);
        assert_eq!(ids_of(&artifacts), vec!["abc123def456"]);
    }

    #[test]
    fn touches_confidence_is_exact_when_only_id_matches_present() {
        let known = vec![
            doc("aaaaaaaaaaaa", "rare/path-name-not-in-text.html"),
            doc("bbbbbbbbbbbb", "another/path.html"),
        ];
        let transcript = "aaaaaaaaaaaa and bbbbbbbbbbbb are both quoted";
        let (artifacts, conf) = extract_touched_ids(transcript, &known);
        assert_eq!(ids_of(&artifacts), vec!["aaaaaaaaaaaa", "bbbbbbbbbbbb"]);
        assert!(artifacts
            .iter()
            .all(|a| a.confidence == TouchesConfidence::Exact));
        assert_eq!(conf, TouchesConfidence::Exact);
    }

    #[test]
    fn touches_per_artifact_confidence_is_mixed_when_scan_has_both_tiers() {
        // CT-A6 — a scan can find some rows via literal id AND other rows
        // only via path substring; per-row confidence must reflect each
        // row's OWN join-tier even though the aggregate rolls up to Fuzzy.
        let known = vec![
            doc("abc123def456", "notes/exact-hit.html"),
            doc("000000000000", "notes/fuzzy-hit.html"),
        ];
        let transcript = "Read /a/work/abc123def456, then notes/fuzzy-hit.html";
        let (artifacts, conf) = extract_touched_ids(transcript, &known);
        assert_eq!(
            conf,
            TouchesConfidence::Fuzzy,
            "aggregate rolls up to Fuzzy"
        );
        let exact_row = artifacts
            .iter()
            .find(|a| a.id == "abc123def456")
            .expect("exact-hit row present");
        assert_eq!(exact_row.confidence, TouchesConfidence::Exact);
        let fuzzy_row = artifacts
            .iter()
            .find(|a| a.id == "000000000000")
            .expect("fuzzy-hit row present");
        assert_eq!(fuzzy_row.confidence, TouchesConfidence::Fuzzy);
    }

    // --- parse_session_activity (S1) ----------------------------------------

    #[test]
    fn activity_prefers_typed_prompt_over_earlier_command_wrapper() {
        // The bug this fixes: on modern transcripts a `<command-name>/clear`
        // (promptSource:null) precedes the real typed prompt; the index used
        // to surface the wrapper. promptSource=="typed" must win.
        let jsonl = concat!(
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>noise</local-command-caveat>"}}"#,
            "\n",
            r#"{"type":"user","promptSource":null,"message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"review the sessions please"}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(
            act.first_user_prompt.as_deref(),
            Some("review the sessions please")
        );
    }

    #[test]
    fn activity_falls_back_when_no_promptsource_present() {
        // Old (v2.1.148) transcripts predate promptSource: the wrapper-skip
        // fallback must still find the real prompt.
        let jsonl = concat!(
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<system-reminder>ctx</system-reminder>"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"hello there"}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.first_user_prompt.as_deref(), Some("hello there"));
    }

    // --- closing_assistant_text (R3) ----------------------------------------

    /// One assistant record carrying a single text block.
    fn assistant_text(text: &str) -> String {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"message":{{"role":"assistant","model":"claude-fable-5","content":[{{"type":"text","text":{}}}]}}}}"#,
            serde_json::Value::String(text.to_string())
        )
    }

    const CLOSING_SUMMARY: &str =
        "Done. Shipped the parser fix, reindexed the corpus, and verified the two goldens.";

    #[test]
    fn closing_text_ends_on_assistant_prose() {
        let jsonl = [
            r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"fix the parser"}}"#.to_string(),
            assistant_text("On it — reading the parser now."),
            assistant_text(CLOSING_SUMMARY),
        ]
        .join("\n");
        assert_eq!(
            closing_assistant_text(&jsonl).as_deref(),
            Some(CLOSING_SUMMARY)
        );
    }

    #[test]
    fn closing_text_survives_a_user_and_tool_result_tail() {
        // The common shape: the closure is N records from the end, buried
        // under tool traffic and a final human line (64% of sessions end on
        // assistant, 35% on user — breadth map).
        let jsonl = [
            assistant_text(CLOSING_SUMMARY),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"just ci"}}]}}"#.to_string(),
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#.to_string(),
            r#"{"type":"user","promptSource":"typed","message":{"role":"user","content":"thanks, that's all"}}"#.to_string(),
        ]
        .join("\n");
        assert_eq!(
            closing_assistant_text(&jsonl).as_deref(),
            Some(CLOSING_SUMMARY)
        );
    }

    #[test]
    fn closing_text_skips_thinking_only_and_tool_only_tail() {
        // The requestId-shatter tail: one reply splits into a prose card, a
        // tool_use card and an EMPTY thinking card (100% of thinking blocks
        // are empty on Fable-5 captures). Only the prose is a closure.
        let jsonl = [
            assistant_text(CLOSING_SUMMARY),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"   ","signature":"sig"}]}}"#.to_string(),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t9","name":"Read","input":{"file_path":"/tmp/x"}}]}}"#.to_string(),
        ]
        .join("\n");
        assert_eq!(
            closing_assistant_text(&jsonl).as_deref(),
            Some(CLOSING_SUMMARY)
        );
    }

    #[test]
    fn closing_text_skips_interrupt_tail_and_synthetic_records() {
        // Interrupt markers are USER-role in real captures (so the
        // assistant-only scan skips them anyway); rate-limit / API-error
        // records ARE assistant-role and must be skipped by the typed rule.
        let jsonl = [
            assistant_text(CLOSING_SUMMARY),
            r#"{"type":"user","message":{"role":"user","content":"[Request interrupted by user for tool use]"}}"#.to_string(),
            r#"{"type":"assistant","isApiErrorMessage":true,"message":{"role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"You've hit your monthly spend limit · raise it at claude.ai/settings/usage"}]}}"#.to_string(),
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","model":"claude-fable-5","content":[{"type":"text","text":"Subagent report: mapped every route in the router and wrote the summary."}]}}"#.to_string(),
        ]
        .join("\n");
        assert_eq!(
            closing_assistant_text(&jsonl).as_deref(),
            Some(CLOSING_SUMMARY),
            "sidechain + synthetic + interrupt records are not the closure"
        );
    }

    #[test]
    fn closing_text_prefers_the_substantial_pass_over_a_terse_tail() {
        let jsonl = [
            assistant_text(CLOSING_SUMMARY),
            assistant_text("Now the index line:"),
        ]
        .join("\n");
        assert_eq!(
            closing_assistant_text(&jsonl).as_deref(),
            Some(CLOSING_SUMMARY)
        );
    }

    #[test]
    fn closing_text_falls_back_to_a_terse_closure() {
        let jsonl = [assistant_text("Reading the file."), assistant_text("Done.")].join("\n");
        assert_eq!(closing_assistant_text(&jsonl).as_deref(), Some("Done."));
    }

    #[test]
    fn closing_text_none_for_a_husk_capture() {
        // The trivial husk (P10): [mode] + caveat + /clear, no assistant prose.
        let jsonl = concat!(
            r#"{"type":"mode","mode":"normal","sessionId":"s1"}"#,
            "\n",
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>Caveat: the messages below were generated…</local-command-caveat>"}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            "\n",
        );
        assert_eq!(closing_assistant_text(jsonl), None);
    }

    #[test]
    fn closing_text_joins_text_blocks_and_ignores_wrapper_prose() {
        let jsonl = [
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"line one"},{"type":"thinking","thinking":""},{"type":"text","text":"line two"}]}}"#.to_string(),
            assistant_text("<local-command-stdout>Set model to Fable 5</local-command-stdout>"),
        ]
        .join("\n");
        assert_eq!(
            closing_assistant_text(&jsonl).as_deref(),
            Some("line one\nline two")
        );
    }

    // --- active_secs (R6/D4) -------------------------------------------------

    #[test]
    fn active_secs_clamps_every_long_gap() {
        // A 6-day multi-gap shape: three short bursts separated by an
        // overnight gap and a two-day gap. Wall clock ≈ 6 days; the honest
        // answer is the bursts plus one clamp per gap.
        const DAY: i64 = 86_400;
        let t: Vec<i64> = vec![
            0,
            30,
            90,                 // burst 1: 30 + 60
            90 + DAY,           // overnight gap → clamp
            90 + DAY + 5,       // burst 2: 5
            90 + 3 * DAY,       // two-day gap → clamp
            90 + 3 * DAY + 240, // burst 3: 240 (under the clamp, counted whole)
            90 + 6 * DAY,       // three-day gap → clamp
        ];
        let expected = 30
            + 60
            + ACTIVE_DELTA_CLAMP_SECS
            + 5
            + ACTIVE_DELTA_CLAMP_SECS
            + 240
            + ACTIVE_DELTA_CLAMP_SECS;
        assert_eq!(active_secs(&t), expected);
        assert!(
            active_secs(&t) < 6 * DAY,
            "active time must not read as wall-clock span"
        );
    }

    #[test]
    fn active_secs_handles_degenerate_and_out_of_order_input() {
        assert_eq!(active_secs(&[]), 0);
        assert_eq!(active_secs(&[1_700_000_000]), 0);
        // Out-of-order (clock skew / a re-ordered capture) contributes 0, never
        // a negative — and never inflates.
        assert_eq!(active_secs(&[100, 40, 70]), 30);
        // Duplicate timestamps (sub-second events) contribute 0.
        assert_eq!(active_secs(&[5, 5, 5]), 0);
        // Exactly at the clamp boundary.
        assert_eq!(
            active_secs(&[0, ACTIVE_DELTA_CLAMP_SECS]),
            ACTIVE_DELTA_CLAMP_SECS
        );
    }

    #[test]
    fn activity_extracts_modal_cwd_branch_and_title() {
        let jsonl = concat!(
            r#"{"type":"user","cwd":"/home/user/project/kb","gitBranch":"main","message":{"role":"user","content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","cwd":"/home/user/project/kb","message":{"role":"assistant","content":[]}}"#,
            "\n",
            r#"{"type":"assistant","cwd":"/home/user/project/kb/web","message":{"role":"assistant","content":[]}}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"first title"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"refined title"}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.cwd.as_deref(), Some("/home/user/project/kb")); // modal (2 vs 1)
        assert_eq!(
            act.all_cwds,
            vec!["/home/user/project/kb", "/home/user/project/kb/web"]
        );
        assert_eq!(act.git_branch.as_deref(), Some("main"));
        assert_eq!(act.ai_title.as_deref(), Some("refined title")); // LAST wins
    }

    #[test]
    fn activity_collects_file_touches_with_actions() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/x.rs"}}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/a/y.rs","old_string":"a","new_string":"b"}}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Write","input":{"file_path":"/a/z.rs","content":"c"}}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(
            act.files,
            vec![
                FileTouch {
                    path: "/a/x.rs".into(),
                    action: FileAction::Read
                },
                FileTouch {
                    path: "/a/y.rs".into(),
                    action: FileAction::Edit
                },
                FileTouch {
                    path: "/a/z.rs".into(),
                    action: FileAction::Write
                },
            ]
        );
        assert_eq!(act.files_read_count(), 1);
    }

    #[test]
    fn activity_dedups_repeated_touches_of_same_path_and_action() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/x.rs"}}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/a/x.rs"}}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.files.len(), 1);
    }

    #[test]
    fn activity_takes_last_file_history_snapshot_as_edited_set() {
        let jsonl = concat!(
            r#"{"type":"file-history-snapshot","snapshot":{"trackedFileBackups":{"a.html":{}}}}"#,
            "\n",
            r#"{"type":"file-history-snapshot","snapshot":{"trackedFileBackups":{"a.html":{},"b.md":{}}}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        let mut edited = act.edited_paths.clone();
        edited.sort();
        assert_eq!(edited, vec!["a.html".to_string(), "b.md".to_string()]);
        assert_eq!(act.files_edited_count(), 2); // authoritative from snapshot
    }

    #[test]
    fn activity_extracts_decisions_effort_and_errors() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50},"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            "\n",
            r#"{"type":"user","toolUseResult":{"answers":{"Scope?":"Full","Lenses?":"A, B"},"questions":[{"question":"Scope?"},{"question":"Lenses?"}]},"message":{"role":"user","content":[{"type":"tool_result","content":"Your questions have been answered: ..."}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","is_error":true,"content":"boom"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"User has approved your plan. Proceed."}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(act.token_total, 150);
        assert_eq!(act.tool_calls, 1);
        assert_eq!(act.error_count, 1);
        // Two question decisions (ordered via questions[]) + one plan approval.
        assert_eq!(act.decisions.len(), 3);
        assert_eq!(act.decisions[0].kind, "question");
        assert_eq!(act.decisions[0].prompt, "Scope?");
        assert_eq!(act.decisions[0].answer.as_deref(), Some("Full"));
        assert_eq!(act.decisions[1].answer.as_deref(), Some("A, B")); // multiSelect joined
        assert_eq!(act.decisions[2].kind, "plan");
    }

    /// W0.2 — a completed (SYNCHRONOUS) Agent/Task delegation's `toolUseResult`
    /// contributes its totals; the real field names below
    /// (`totalTokens`/`totalToolUseCount`/`toolStats.editFileCount`) match
    /// `subagent-delegation.jsonl` verbatim, not a guess.
    #[test]
    fn activity_extracts_completed_subagent_stats() {
        let jsonl = concat!(
            r#"{"type":"user","toolUseResult":{"status":"completed","agentId":"a1","agentType":"Explore","totalTokens":98275,"totalToolUseCount":40,"toolStats":{"readCount":15,"searchCount":0,"bashCount":25,"editFileCount":3,"linesAdded":0,"linesRemoved":0,"otherToolCount":0}},"message":{"role":"user","content":[{"type":"tool_result","content":"done"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.subagent_count, 1);
        assert_eq!(act.subagent_tokens, 98_275);
        assert_eq!(act.subagent_tool_calls, 40);
        assert_eq!(act.subagent_files_edited, 3);
        assert_eq!(act.subagent_launched_unstatted, 0);
    }

    /// W0.2 — an ASYNC/BACKGROUND delegation (the harness default) leaves
    /// only a `status:"async_launched"` stub with no stats attached (the
    /// shape `tool-heavy-research.jsonl` models). It must land in
    /// `subagent_launched_unstatted`, NOT contribute a zero-valued
    /// `subagent_count` — a zero must never masquerade as "no subagents ran".
    #[test]
    fn activity_counts_async_stub_as_launched_unstatted() {
        let jsonl = concat!(
            r#"{"type":"user","toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"a2","description":"hunt bugs","resolvedModel":"claude-opus-4-8","prompt":"…","outputFile":"/tmp/out.md","canReadOutputFile":true},"message":{"role":"user","content":[{"type":"tool_result","content":"launched"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.subagent_count, 0);
        assert_eq!(act.subagent_tokens, 0);
        assert_eq!(act.subagent_launched_unstatted, 1);
    }

    /// W0.2 — a session that mixes completed + async-launched subagents sums
    /// the completed ones and tallies the rest separately, never conflating
    /// the two buckets.
    #[test]
    fn activity_sums_multiple_completed_subagents_and_tallies_unstatted_separately() {
        let jsonl = concat!(
            r#"{"type":"user","toolUseResult":{"status":"completed","agentId":"a1","totalTokens":100,"totalToolUseCount":10,"toolStats":{"editFileCount":1}},"message":{"role":"user","content":[]}}"#,
            "\n",
            r#"{"type":"user","toolUseResult":{"status":"completed","agentId":"a2","totalTokens":200,"totalToolUseCount":20,"toolStats":{"editFileCount":2}},"message":{"role":"user","content":[]}}"#,
            "\n",
            r#"{"type":"user","toolUseResult":{"status":"async_launched","agentId":"a3"},"message":{"role":"user","content":[]}}"#,
            "\n",
            // A non-Agent tool result (no agentId) must not be mistaken for either bucket.
            r#"{"type":"user","toolUseResult":{"stdout":"ok","stderr":"","interrupted":false},"message":{"role":"user","content":[]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.subagent_count, 2);
        assert_eq!(act.subagent_tokens, 300);
        assert_eq!(act.subagent_tool_calls, 30);
        assert_eq!(act.subagent_files_edited, 3);
        assert_eq!(act.subagent_launched_unstatted, 1);
    }

    /// W0.2 — whole-transcript regression over the (synthetic)
    /// `tests/session_fixtures/` corpus: `subagent-delegation.jsonl` carries
    /// exactly ONE completed sync delegation (98,275 tokens / 40 tool calls /
    /// 0 files edited — read off the fixture, not guessed);
    /// `tool-heavy-research.jsonl` carries exactly TWO async-launched stubs
    /// and zero completed ones. These numbers are the fixture set's contract:
    /// they survived the 2026-09 re-synthesis unchanged.
    #[test]
    fn activity_subagent_stats_from_transcript_fixtures() {
        let fixture = |name: &str| -> String {
            std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/session_fixtures")
                    .join(format!("{name}.jsonl")),
            )
            .unwrap()
        };

        let act = parse_session_activity(&fixture("subagent-delegation"));
        assert_eq!(act.subagent_count, 1);
        assert_eq!(act.subagent_tokens, 98_275);
        assert_eq!(act.subagent_tool_calls, 40);
        assert_eq!(act.subagent_files_edited, 0);
        assert_eq!(act.subagent_launched_unstatted, 0);

        let act = parse_session_activity(&fixture("tool-heavy-research"));
        assert_eq!(act.subagent_count, 0);
        assert_eq!(act.subagent_tokens, 0);
        assert_eq!(act.subagent_launched_unstatted, 2);

        // ask-user-question.jsonl carries 3 completed sync delegations —
        // exercises summation across more than 2 agents.
        let act = parse_session_activity(&fixture("ask-user-question"));
        assert_eq!(act.subagent_count, 3);
        assert_eq!(act.subagent_tokens, 42_968 + 52_473 + 65_620);
        assert_eq!(act.subagent_tool_calls, 28 + 27 + 35);
        assert_eq!(act.subagent_launched_unstatted, 0);
    }

    #[test]
    fn activity_extracts_git_commits_with_sha() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"git add -A && git commit -m \"feat: ship it\""}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"[main abc1234] feat: ship it\n 3 files changed"}]}}"#,
            "\n",
            // A read-only git command must NOT be recorded.
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b2","name":"Bash","input":{"command":"git status"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b2","content":"On branch main"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.commits.len(), 1);
        assert_eq!(act.commits[0].kind, "commit");
        assert_eq!(act.commits[0].sha.as_deref(), Some("abc1234"));
        assert_eq!(act.commits[0].subject.as_deref(), Some("feat: ship it"));
    }

    /// A chained `git commit … && git push` from ONE Bash call must yield
    /// BOTH a commit event and a push event — the old `git_action_of`
    /// returned on the first segment match and silently lost the push.
    #[test]
    fn activity_parses_commit_and_push_in_one_chained_command() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"git add -A && git commit -m \"feat: ship\" && git push"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"[main abc1234] feat: ship\n 1 file changed\nTo github.com:x/y.git\n   abc1234..def5678  main -> main"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.commits.len(), 2, "{:?}", act.commits);
        assert_eq!(act.commits[0].kind, "commit");
        assert_eq!(act.commits[0].subject.as_deref(), Some("feat: ship"));
        assert_eq!(act.commits[1].kind, "push");
    }

    /// Two SEPARATE Bash calls (e.g. a retried `git push` after a flaky
    /// network blip) that both parse to the same `(kind, sha)` must collapse
    /// into ONE row, not two.
    #[test]
    fn activity_dedupes_repeated_kind_sha_pairs() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"git push"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"To github.com:x/y.git\n   abc1234..def5678  main -> main"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b2","name":"Bash","input":{"command":"git push"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b2","content":"To github.com:x/y.git\n   abc1234..def5678  main -> main"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.commits.len(), 1, "{:?}", act.commits);
        assert_eq!(act.commits[0].kind, "push");
        assert_eq!(act.commits[0].sha.as_deref(), Some("abc1234"));
    }

    /// Two DISTINCT commits whose tool results both fail to yield a
    /// parseable sha (`sha: None`) must NOT collapse into one — `None` is
    /// "unknown", not an identity, so the (kind, sha) dedup only applies
    /// when a sha was actually recovered.
    #[test]
    fn activity_keeps_distinct_commits_with_unrecovered_shas() {
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"git commit -m \"feat: first\""}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"2 files changed, 10 insertions"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b2","name":"Bash","input":{"command":"git commit -m \"feat: second\""}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b2","content":"3 files changed, 4 insertions"}]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        assert_eq!(act.commits.len(), 2, "{:?}", act.commits);
        assert!(act
            .commits
            .iter()
            .all(|c| c.kind == "commit" && c.sha.is_none()));
        assert_eq!(act.commits[0].subject.as_deref(), Some("feat: first"));
        assert_eq!(act.commits[1].subject.as_deref(), Some("feat: second"));
    }

    #[test]
    fn thread_boundaries_splits_on_large_gaps() {
        let day = 86_400;
        // Three sessions close together, then a 5-day gap, then two close.
        let spans = vec![
            (0, 100),
            (day, day + 100),               // +1 day → same thread
            (2 * day, 2 * day + 50),        // +1 day → same thread
            (8 * day, 8 * day + 50),        // +6 days → NEW thread
            (8 * day + 200, 8 * day + 300), // close → same thread
        ];
        let bounds = thread_boundaries(&spans, THREAD_GAP_SECS);
        assert_eq!(bounds, vec![3], "one split, before the 4th session");
    }

    #[test]
    fn thread_boundaries_empty_and_single() {
        assert!(thread_boundaries(&[], THREAD_GAP_SECS).is_empty());
        assert!(thread_boundaries(&[(0, 10)], THREAD_GAP_SECS).is_empty());
    }

    #[test]
    fn parse_git_sha_skips_decimal_counts() {
        // "3 files changed" must not yield "3" or a decimal token as a SHA.
        assert_eq!(parse_git_sha("3 files changed, 10 insertions"), None);
        assert_eq!(parse_git_sha("[main deadbee] x"), Some("deadbee".into()));
    }

    #[test]
    fn git_action_of_rejects_commit_tree_but_accepts_real_commits() {
        let kinds =
            |cmd: &str| -> Vec<String> { git_action_of(cmd).into_iter().map(|(k, _)| k).collect() };
        assert!(git_action_of("git commit-tree $T").is_empty());
        assert!(git_action_of("git commit-graph write").is_empty());
        assert!(git_action_of("git status").is_empty());
        assert!(git_action_of("git log --oneline").is_empty());
        assert_eq!(kinds("git commit"), vec!["commit".to_string()]);
        assert_eq!(
            kinds("git commit --amend --no-edit"),
            vec!["commit".to_string()]
        );
        assert_eq!(kinds("git push origin main"), vec!["push".to_string()]);
    }

    /// GC-B8 — a `git commit -m "$(cat <<'EOF' … EOF)"` heredoc (the
    /// canonical multi-line commit convention this very repo's CLAUDE.md
    /// prescribes) must yield the body's first line as the subject, not the
    /// literal `$(cat <<'EOF'` text. The chained `&& git push` must ALSO be
    /// recognised (B5 — chained segments are no longer lost).
    #[test]
    fn git_action_of_reads_heredoc_commit_subject_single_quoted_delim() {
        let cmd = "git add x && git commit -m \"$(cat <<'EOF'\nfix: real subject line\n\nlonger body\nCo-Authored-By: someone\nEOF\n)\" && git push";
        let actions = git_action_of(cmd);
        let (kind, subject) = actions
            .iter()
            .find(|(k, _)| k == "commit")
            .expect("recognised as a commit");
        assert_eq!(kind, "commit");
        assert_eq!(subject.as_deref(), Some("fix: real subject line"));
        assert!(
            actions.iter().any(|(k, _)| k == "push"),
            "the chained push must not be lost: {actions:?}"
        );
    }

    /// Same shape but the delimiter is double-quoted (`<<"EOF"`) — the other
    /// quoted heredoc form in common use.
    #[test]
    fn git_action_of_reads_heredoc_commit_subject_double_quoted_delim() {
        let cmd = "git commit -m \"$(cat <<\"EOF\"\nchore: another subject\nEOF\n)\"";
        let actions = git_action_of(cmd);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].0, "commit");
        assert_eq!(actions[0].1.as_deref(), Some("chore: another subject"));
    }

    /// Bare, unquoted delimiter (`<<EOF`, allows shell expansion inside the
    /// body) is also a heredoc — same body-first-line extraction applies.
    #[test]
    fn git_action_of_reads_heredoc_commit_subject_unquoted_delim() {
        let cmd = "git commit -m \"$(cat <<EOF\ndocs: unquoted delimiter subject\nEOF\n)\"";
        let actions = git_action_of(cmd);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].0, "commit");
        assert_eq!(
            actions[0].1.as_deref(),
            Some("docs: unquoted delimiter subject")
        );
    }

    /// The plain `-m "subject"` fast path (no heredoc at all) must keep
    /// working unchanged.
    #[test]
    fn git_action_of_reads_plain_quoted_commit_subject() {
        let actions = git_action_of(r#"git commit -m "plain subject""#);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].0, "commit");
        assert_eq!(actions[0].1.as_deref(), Some("plain subject"));
    }

    /// B5.1 — `git commit -F - <<'EOF' … EOF` (stdin-heredoc form): the
    /// message is read from stdin, which is fed by a heredoc redirected
    /// directly onto the command (not wrapped in `$(cat …)` like `-m`). The
    /// body's first non-empty line is the recovered subject.
    #[test]
    fn git_action_of_reads_dash_f_stdin_heredoc_subject() {
        let cmd = "git commit -F - <<'EOF' && git push\nfeat: subject via stdin heredoc\n\nbody line\nEOF\n";
        let actions = git_action_of(cmd);
        let (kind, subject) = actions
            .iter()
            .find(|(k, _)| k == "commit")
            .expect("recognised as a commit");
        assert_eq!(kind, "commit");
        assert_eq!(subject.as_deref(), Some("feat: subject via stdin heredoc"));
        assert!(
            actions.iter().any(|(k, _)| k == "push"),
            "the chained push must not be lost: {actions:?}"
        );
    }

    /// B5.1 — plain `-F <file>` (no heredoc — the message lives in a file kb
    /// never sees) is UNRECOVERABLE: the segment is still classified as a
    /// commit (kind = "commit"), but the subject must be `None`, never a
    /// fabricated placeholder.
    #[test]
    fn git_action_of_dash_f_file_is_unrecoverable_but_flagged() {
        let actions = git_action_of("git commit -F /tmp/commit-body.txt");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].0, "commit");
        assert_eq!(
            actions[0].1, None,
            "a -F <file> subject must never be invented"
        );
    }

    /// B5.2 — every `&&`-chained git segment must be classified, not just
    /// the first. `git add` isn't itself a tracked VCS action, so the chain
    /// yields exactly the commit + push pair, in segment order.
    #[test]
    fn git_action_of_parses_every_chained_segment() {
        let actions = git_action_of(r#"git add -A && git commit -m "ship it" && git push"#);
        assert_eq!(actions.len(), 2, "{actions:?}");
        assert_eq!(actions[0].0, "commit");
        assert_eq!(actions[0].1.as_deref(), Some("ship it"));
        assert_eq!(actions[1].0, "push");
    }

    /// B5.4 — `git tag` in LIST mode (bare, `-l`, `--list`) is a read, not a
    /// write; only an actual tag CREATE (`git tag v1.0`) is a write.
    #[test]
    fn git_action_of_tag_list_mode_is_a_read_create_is_a_write() {
        assert!(git_action_of("git tag").is_empty(), "bare `git tag` lists");
        assert!(git_action_of("git tag -l").is_empty());
        assert!(git_action_of("git tag -l 'v1.*'").is_empty());
        assert!(git_action_of("git tag --list").is_empty());
        let actions = git_action_of("git tag v1.0");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].0, "tag");
    }

    #[test]
    fn activity_ended_at_is_the_max_timestamp() {
        let jsonl = concat!(
            r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"a"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-05-27T09:30:00Z","message":{"role":"assistant","content":[]}}"#,
            "\n",
        );
        let act = parse_session_activity(jsonl);
        // 2026-05-27 09:30:00 UTC.
        assert_eq!(act.ended_at, parse_iso_utc("2026-05-27T09:30:00Z"));
        assert!(act.ended_at.unwrap() > parse_iso_utc("2026-05-27T08:00:00Z").unwrap());
    }

    // --- corpus resolution (S3) ---------------------------------------------

    #[test]
    fn resolve_corpus_path_empty_mounts_is_out_of_corpus() {
        let (in_c, kb, id) = resolve_corpus_path("/anything/x.html", None, &[]);
        assert!(!in_c);
        assert!(kb.is_none() && id.is_none());
    }

    #[test]
    fn resolve_corpus_path_absolute_under_a_mount_resolves_to_artifact_id() {
        let tmp = std::env::temp_dir().join(format!("kbtest-resolve-{}", std::process::id()));
        let root = tmp.join("research");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        let file = root.join("notes/a.html");
        std::fs::write(&file, b"<html></html>").unwrap();
        let mounts = vec![CorpusMount {
            kb: "research".into(),
            source_root: root.clone(),
        }];
        let (in_c, kb, id) = resolve_corpus_path(&file.to_string_lossy(), None, &mounts);
        assert!(in_c);
        assert_eq!(kb.as_deref(), Some("research"));
        // Id is the source-relative id the indexer would assign.
        let expect = crate::ids::ArtifactId::from_path("notes/a.html")
            .as_str()
            .to_string();
        assert_eq!(id.as_deref(), Some(expect.as_str()));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn resolve_corpus_path_relative_uses_cwd() {
        let tmp = std::env::temp_dir().join(format!("kbtest-resolvecwd-{}", std::process::id()));
        let root = tmp.join("research");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("b.html"), b"x").unwrap();
        let mounts = vec![CorpusMount {
            kb: "research".into(),
            source_root: root.clone(),
        }];
        // cwd = the corpus root; relative path "b.html" resolves under it.
        let (in_c, kb, _id) = resolve_corpus_path("b.html", Some(&root.to_string_lossy()), &mounts);
        assert!(in_c);
        assert_eq!(kb.as_deref(), Some("research"));
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn resolve_corpus_path_outside_all_mounts_is_out_of_corpus() {
        let tmp = std::env::temp_dir().join(format!("kbtest-resolveout-{}", std::process::id()));
        let root = tmp.join("research");
        std::fs::create_dir_all(&root).unwrap();
        let mounts = vec![CorpusMount {
            kb: "research".into(),
            source_root: root.clone(),
        }];
        let (in_c, _, _) = resolve_corpus_path("/etc/hosts", None, &mounts);
        assert!(!in_c);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn set_and_get_corpus_mounts_sorts_specific_first() {
        let _g = MOUNT_TEST_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        set_corpus_mounts(vec![
            CorpusMount {
                kb: "a".into(),
                source_root: PathBuf::from("/srv/a"),
            },
            CorpusMount {
                kb: "ab".into(),
                source_root: PathBuf::from("/srv/a/b"),
            },
        ]);
        let m = corpus_mounts();
        // Longer (more specific) root first.
        assert_eq!(m[0].kb, "ab");
        assert_eq!(m[1].kb, "a");
        // Reset so other tests see an empty table.
        set_corpus_mounts(vec![]);
    }

    #[test]
    fn parse_session_html_full_threads_activity_through() {
        let jsonl = concat!(
            r#"{"type":"user","cwd":"/p/kb","gitBranch":"main","promptSource":"typed","message":{"role":"user","content":"do the thing"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/p/kb/a.rs"}}]}}"#,
            "\n",
        );
        let html = wrapper("sid", jsonl);
        let parse = parse_session_html_full(&html, "session-20260524T100000Z-sid.html", 0);
        assert_eq!(parse.facts.session_id, "sid");
        assert_eq!(
            parse.facts.first_user_prompt.as_deref(),
            Some("do the thing")
        );
        assert_eq!(parse.activity.cwd.as_deref(), Some("/p/kb"));
        assert_eq!(parse.activity.git_branch.as_deref(), Some("main"));
        assert_eq!(parse.activity.files.len(), 1);
    }

    // --- commits envelope block (W0.4) --------------------------------------

    #[test]
    fn render_commits_block_absent_when_empty() {
        assert_eq!(render_commits_block(&[]), "");
    }

    #[test]
    fn render_and_extract_commits_block_round_trips() {
        let commits = vec![
            CapturedCommit {
                kind: "commit".to_string(),
                sha: Some("abc1234".to_string()),
                subject: Some("feat: real subject".to_string()),
                resolved: true,
                sha_full: Some("abc1234def5678".to_string()),
                repo_root: Some("/home/u/proj".to_string()),
                author: Some("kb-test <test@kb>".to_string()),
                parents: Some(1),
                trailers: vec!["Kb-Session: sid-1".to_string()],
            },
            CapturedCommit {
                kind: "push".to_string(),
                sha: None,
                subject: Some("push".to_string()),
                resolved: false,
                ..Default::default()
            },
        ];
        let block = render_commits_block(&commits);
        assert!(block.starts_with(&format!(
            r#"<script type="application/json" id="{COMMITS_BLOCK_ID}">"#
        )));
        assert!(block.ends_with("</script>"));

        let html = format!("<html><body><pre>ignored</pre>\n{block}\n</body></html>");
        let got = extract_commits_block(&html).expect("block parses");
        assert_eq!(got, commits);
    }

    #[test]
    fn extract_commits_block_absent_returns_none() {
        let html = "<html><body><pre>no commits here</pre></body></html>";
        assert!(extract_commits_block(html).is_none());
    }

    #[test]
    fn extract_commits_block_malformed_returns_none_not_panic() {
        let html = r#"<html><body><pre>x</pre>
<script type="application/json" id="kb-session-commits">not valid json</script>
</body></html>"#;
        assert!(extract_commits_block(html).is_none());
    }

    // --- CT-F1: `Kb-Memory:` trailer parse-back ---------------------------

    #[test]
    fn memory_ids_from_trailers_reads_the_ratified_grammar() {
        let got = memory_ids_from_trailers(&[
            "Kb-Session: sess-1".to_string(),
            "Kb-Memory: abc123def456".to_string(),
            "Co-Authored-By: Claude <noreply@anthropic.com>".to_string(),
            "kb-memory: 0123456789ab".to_string(), // key is case-insensitive
        ]);
        assert_eq!(got.ids, vec!["abc123def456", "0123456789ab"]);
        assert_eq!(got.malformed, 0);
    }

    #[test]
    fn memory_ids_from_trailers_is_empty_without_a_kb_memory_trailer() {
        // The overwhelmingly common case: the repo never opted in. An empty
        // parse is a NON-signal, not a malformed one.
        let got = memory_ids_from_trailers(&[
            "Kb-Session: sess-1".to_string(),
            "Signed-off-by: Someone <s@example.com>".to_string(),
        ]);
        assert!(got.ids.is_empty());
        assert_eq!(got.malformed, 0);
        assert_eq!(memory_ids_from_trailers(&[]), MemoryTrailerParse::default());
    }

    #[test]
    fn memory_ids_from_trailers_counts_malformed_ids_and_stores_none_of_them() {
        let got = memory_ids_from_trailers(&[
            "Kb-Memory: ABC123DEF456".to_string(),          // uppercase
            "Kb-Memory: abc123".to_string(),                // too short
            "Kb-Memory: abc123def4567".to_string(),         // too long
            "Kb-Memory: zzz123def456".to_string(),          // non-hex
            "Kb-Memory:".to_string(),                       // empty
            "Kb-Memory: abc123def456 (a note)".to_string(), // trailing junk
            "Kb-Memory: 0123456789ab".to_string(),          // the one good one
        ]);
        assert_eq!(got.ids, vec!["0123456789ab"]);
        assert_eq!(got.malformed, 6);
    }

    #[test]
    fn memory_ids_from_trailers_collapses_duplicates_in_first_seen_order() {
        let got = memory_ids_from_trailers(&[
            "Kb-Memory: bbbbbbbbbbbb".to_string(),
            "Kb-Memory: aaaaaaaaaaaa".to_string(),
            "Kb-Memory: bbbbbbbbbbbb".to_string(),
        ]);
        assert_eq!(got.ids, vec!["bbbbbbbbbbbb", "aaaaaaaaaaaa"]);
        assert_eq!(got.malformed, 0);
    }

    #[test]
    fn memory_ids_from_trailers_ignores_a_key_that_merely_contains_the_word() {
        // Only the exact key matches — never a substring/prefix.
        let got = memory_ids_from_trailers(&[
            "X-Kb-Memory: abc123def456".to_string(),
            "Kb-Memory-Note: abc123def456".to_string(),
        ]);
        assert!(got.ids.is_empty());
        assert_eq!(got.malformed, 0);
    }

    #[test]
    fn render_commits_block_escapes_embedded_closing_script_tag() {
        // A commit subject that literally contains `</script>` must never be
        // able to prematurely close the block.
        let commits = vec![CapturedCommit {
            kind: "commit".to_string(),
            subject: Some("</script><script>alert(1)</script>".to_string()),
            ..Default::default()
        }];
        let block = render_commits_block(&commits);
        assert!(
            !block[r#"<script type="application/json" id="kb-session-commits">"#.len()..]
                .contains("</script>alert"),
            "the payload's closing tag must be escaped: {block}"
        );
        let html = format!("<pre>x</pre>\n{block}\n");
        let got = extract_commits_block(&html).expect("still parses");
        assert_eq!(
            got[0].subject.as_deref(),
            Some("</script><script>alert(1)</script>")
        );
    }

    /// The commits block must never disturb `recover_jsonl_from_capture`'s
    /// byte-identical round-trip of the `<pre>` block — it only ever reads
    /// the FIRST `<pre>…</pre>`, so an additive tail after it must be inert.
    #[test]
    fn commits_block_does_not_disturb_pre_round_trip() {
        let jsonl = "line one\nline <two>&three\n";
        let sid = "sid-rt";
        let base = wrapper(sid, jsonl);
        // Splice a commits block after `</pre>`, mirroring what `kb sessions
        // capture` writes.
        let commits = vec![CapturedCommit {
            kind: "commit".to_string(),
            sha: Some("deadbee".to_string()),
            resolved: true,
            ..Default::default()
        }];
        let block = render_commits_block(&commits);
        let with_tail = base.replacen("</pre>\n", &format!("</pre>\n{block}\n"), 1);

        assert_eq!(
            recover_jsonl_from_capture(&base),
            recover_jsonl_from_capture(&with_tail)
        );
        assert_eq!(
            recover_jsonl_from_capture(&with_tail).as_deref(),
            Some(jsonl)
        );
        assert_eq!(extract_commits_block(&with_tail).unwrap(), commits);
        assert!(extract_commits_block(&base).is_none());
    }

    // --- subagent digest block (W0.5) ---------------------------------------

    #[test]
    fn render_subagents_block_absent_when_empty() {
        assert_eq!(render_subagents_block(&[]), "");
    }

    #[test]
    fn parse_subagent_jsonl_reuses_the_activity_parser() {
        // Same line shapes as the main transcript: a tool_use Edit + Read, a
        // token-bearing assistant turn, and one is_error tool_result.
        let jsonl = concat!(
            r#"{"type":"assistant","message":{"role":"assistant","model":"claude","usage":{"input_tokens":100,"output_tokens":50},"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/p/a.rs"}},{"type":"tool_use","name":"Edit","input":{"file_path":"/p/b.rs"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","is_error":true,"content":"boom"}]}}"#,
            "\n",
        );
        let d = parse_subagent_jsonl(jsonl, "a1");
        assert_eq!(d.agent_id, "a1", "no agentId field ⇒ filename fallback");
        assert_eq!(d.tokens, 150);
        assert_eq!(d.tool_calls, 2);
        assert_eq!(d.errors, 1);
        assert_eq!(
            d.files,
            vec![
                SubagentFileEntry {
                    path: "/p/a.rs".to_string(),
                    action: "read".to_string(),
                },
                SubagentFileEntry {
                    path: "/p/b.rs".to_string(),
                    action: "edit".to_string(),
                },
            ]
        );
    }

    #[test]
    fn parse_subagent_jsonl_prefers_the_transcripts_own_agent_id() {
        let jsonl =
            r#"{"type":"user","agentId":"real-agent-7","message":{"role":"user","content":"hi"}}"#;
        let d = parse_subagent_jsonl(jsonl, "filename-fallback");
        assert_eq!(d.agent_id, "real-agent-7");
    }

    #[test]
    fn collect_subagent_digests_walks_agent_files_and_skips_workflows_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let subagents_dir = tmp.path().join("sess-1/subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        std::fs::write(
            subagents_dir.join("agent-a1.jsonl"),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/p/x.rs"}}]}}
"#,
        )
        .unwrap();
        std::fs::write(
            subagents_dir.join("agent-a2.jsonl"),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/p/y.rs"}}]}}
"#,
        )
        .unwrap();
        // A workflow journal, sibling to the agent-*.jsonl files — must be
        // ignored (not a *.jsonl matching `agent-*`, and a directory besides).
        let wf_dir = subagents_dir.join("workflows/wf_1");
        std::fs::create_dir_all(&wf_dir).unwrap();
        std::fs::write(wf_dir.join("journal.jsonl"), r#"{"type":"progress"}"#).unwrap();

        let agents = collect_subagent_digests(&subagents_dir);
        assert_eq!(agents.len(), 2, "exactly the two agent-*.jsonl sidecars");
        let ids: Vec<&str> = agents.iter().map(|a| a.agent_id.as_str()).collect();
        assert_eq!(ids, vec!["a1", "a2"], "deterministic, sorted order");
        assert_eq!(agents[0].files[0].path, "/p/x.rs");
        assert_eq!(agents[1].files[0].path, "/p/y.rs");
    }

    #[test]
    fn collect_subagent_digests_missing_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = collect_subagent_digests(&tmp.path().join("nonexistent/subagents"));
        assert!(agents.is_empty());
    }

    #[test]
    fn render_and_extract_subagents_block_round_trips() {
        let agents = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![SubagentFileEntry {
                path: "/p/a.rs".to_string(),
                action: "edit".to_string(),
            }],
            tokens: 500,
            tool_calls: 12,
            errors: 1,
        }];
        let block = render_subagents_block(&agents);
        assert!(block.starts_with(&format!(
            r#"<script type="application/json" id="{SUBAGENTS_BLOCK_ID}">"#
        )));
        assert!(block.ends_with("</script>"));

        let html = format!("<html><body><pre>ignored</pre>\n{block}\n</body></html>");
        let got = extract_subagents_block(&html).expect("block parses");
        assert_eq!(got.agents, agents);
        assert!(!got.truncated);
    }

    #[test]
    fn extract_subagents_block_absent_returns_none() {
        let html = "<html><body><pre>no subagents here</pre></body></html>";
        assert!(extract_subagents_block(html).is_none());
    }

    #[test]
    fn extract_subagents_block_malformed_returns_none_not_panic() {
        let html = r#"<html><body><pre>x</pre>
<script type="application/json" id="kb-session-subagents">not valid json</script>
</body></html>"#;
        assert!(extract_subagents_block(html).is_none());
    }

    /// The 256 KB cap: an oversized synthetic sidecar set (many agents, each
    /// with a long file list) must render UNDER the cap with `truncated:
    /// true`, trimming the largest agents first rather than dropping a whole
    /// agent from the block.
    #[test]
    fn render_subagents_block_caps_and_flags_truncated() {
        let mut agents = Vec::new();
        for a in 0..5 {
            let files = (0..3000)
                .map(|i| SubagentFileEntry {
                    path: format!(
                        "/very/long/synthetic/path/for/coverage/agent-{a}/file-{i:05}.rs"
                    ),
                    action: "edit".to_string(),
                })
                .collect();
            agents.push(SubagentDigest {
                agent_id: format!("agent-{a}"),
                files,
                tokens: 1000,
                tool_calls: 10,
                errors: 0,
            });
        }
        let block = render_subagents_block(&agents);
        assert!(
            block.len() <= SUBAGENTS_BLOCK_CAP_BYTES + 200,
            "block must be capped near {SUBAGENTS_BLOCK_CAP_BYTES} bytes, got {}",
            block.len()
        );
        let html = format!("<pre>x</pre>\n{block}\n");
        let got = extract_subagents_block(&html).expect("still parses");
        assert!(got.truncated, "cap must flag truncated");
        assert_eq!(
            got.agents.len(),
            5,
            "every agent survives — files trimmed, not agents dropped"
        );
        // No agent was emptied while another still holds many files: the
        // trim-largest-first rule keeps the spread roughly even.
        let counts: Vec<usize> = got.agents.iter().map(|a| a.files.len()).collect();
        let max = *counts.iter().max().unwrap();
        let min = *counts.iter().min().unwrap();
        assert!(
            max - min <= 1,
            "trim-the-largest-first should keep counts within 1 of each other: {counts:?}"
        );
    }

    #[test]
    fn render_subagents_block_under_cap_is_not_truncated() {
        let agents = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![SubagentFileEntry {
                path: "/p/a.rs".to_string(),
                action: "read".to_string(),
            }],
            tokens: 10,
            tool_calls: 1,
            errors: 0,
        }];
        let block = render_subagents_block(&agents);
        let html = format!("<pre>x</pre>\n{block}\n");
        let got = extract_subagents_block(&html).unwrap();
        assert!(!got.truncated);
    }

    /// The subagents block must never disturb `recover_jsonl_from_capture`'s
    /// byte-identical round-trip of the `<pre>` block, same contract as the
    /// commits block.
    #[test]
    fn subagents_block_does_not_disturb_pre_round_trip() {
        let jsonl = "line one\nline <two>&three\n";
        let sid = "sid-rt-2";
        let base = wrapper(sid, jsonl);
        let agents = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 5,
            tool_calls: 1,
            errors: 0,
        }];
        let block = render_subagents_block(&agents);
        let with_tail = base.replacen("</pre>\n", &format!("</pre>\n{block}\n"), 1);

        assert_eq!(
            recover_jsonl_from_capture(&base),
            recover_jsonl_from_capture(&with_tail)
        );
        assert_eq!(extract_subagents_block(&with_tail).unwrap().agents, agents);
        assert!(extract_subagents_block(&base).is_none());
    }

    /// `replace_subagents_block` — the `--refresh-subagents` backfill path:
    /// splicing a fresh block into a capture that has NEITHER an existing
    /// subagents block nor a commits block must leave the `<pre>` and the
    /// rest of the shell untouched.
    #[test]
    fn replace_subagents_block_splices_into_a_capture_with_no_existing_block() {
        let sid = "sid-refresh-1";
        let base = wrapper(sid, "hello\n");
        let agents = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![SubagentFileEntry {
                path: "/p/z.rs".to_string(),
                action: "write".to_string(),
            }],
            tokens: 20,
            tool_calls: 2,
            errors: 0,
        }];
        let rewritten = replace_subagents_block(&base, &agents);
        assert_eq!(
            recover_jsonl_from_capture(&base),
            recover_jsonl_from_capture(&rewritten)
        );
        assert_eq!(extract_subagents_block(&rewritten).unwrap().agents, agents);
        // A pre-existing commits block (there is none here) would be
        // untouched too; sanity-check none was accidentally created.
        assert!(extract_commits_block(&rewritten).is_none());
    }

    /// A second `replace_subagents_block` call (simulating a re-run of
    /// `--refresh-subagents`) must be idempotent: the old block is replaced
    /// wholesale, not duplicated or accumulated.
    #[test]
    fn replace_subagents_block_is_idempotent_on_a_second_refresh() {
        let sid = "sid-refresh-2";
        let base = wrapper(sid, "hello\n");
        let agents_v1 = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 1,
            tool_calls: 1,
            errors: 0,
        }];
        let once = replace_subagents_block(&base, &agents_v1);
        let agents_v2 = vec![
            SubagentDigest {
                agent_id: "a1".to_string(),
                files: vec![],
                tokens: 1,
                tool_calls: 1,
                errors: 0,
            },
            SubagentDigest {
                agent_id: "a2".to_string(),
                files: vec![],
                tokens: 2,
                tool_calls: 2,
                errors: 0,
            },
        ];
        let twice = replace_subagents_block(&once, &agents_v2);

        // Exactly one block present, carrying the LATEST agents.
        assert_eq!(twice.matches(SUBAGENTS_BLOCK_ID).count(), 1);
        assert_eq!(extract_subagents_block(&twice).unwrap().agents, agents_v2);
        assert_eq!(
            recover_jsonl_from_capture(&base),
            recover_jsonl_from_capture(&twice)
        );

        // Refreshing a third time against an IDENTICAL agent set must be a
        // true no-op: byte-identical output (idempotent re-capture path).
        let thrice = replace_subagents_block(&twice, &agents_v2);
        assert_eq!(twice, thrice);
    }

    /// `replace_subagents_block` must leave an EXISTING commits block
    /// (written after `</pre>` by `kb sessions capture`) fully intact when
    /// splicing the subagents block in.
    #[test]
    fn replace_subagents_block_preserves_an_existing_commits_block() {
        let sid = "sid-refresh-3";
        let base = wrapper(sid, "hello\n");
        let commits = vec![CapturedCommit {
            kind: "commit".to_string(),
            sha: Some("abc1234".to_string()),
            resolved: true,
            ..Default::default()
        }];
        let commits_block = render_commits_block(&commits);
        let with_commits = base.replacen("</pre>\n", &format!("</pre>\n{commits_block}\n"), 1);

        let agents = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 1,
            tool_calls: 1,
            errors: 0,
        }];
        let rewritten = replace_subagents_block(&with_commits, &agents);

        assert_eq!(extract_commits_block(&rewritten).unwrap(), commits);
        assert_eq!(extract_subagents_block(&rewritten).unwrap().agents, agents);
        assert_eq!(
            recover_jsonl_from_capture(&with_commits),
            recover_jsonl_from_capture(&rewritten)
        );
    }

    /// `agents: []` removes the block entirely rather than writing an empty
    /// one (mirrors the commits block's "absent, not `[]`" contract).
    #[test]
    fn replace_subagents_block_with_no_agents_removes_the_block() {
        let sid = "sid-refresh-4";
        let base = wrapper(sid, "hello\n");
        let agents = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 1,
            tool_calls: 1,
            errors: 0,
        }];
        let with_block = replace_subagents_block(&base, &agents);
        assert!(extract_subagents_block(&with_block).is_some());

        let cleared = replace_subagents_block(&with_block, &[]);
        assert!(extract_subagents_block(&cleared).is_none());
    }

    // --- sidecar-text tail block (W0.6) -------------------------------------

    #[test]
    fn render_sidecar_text_block_absent_when_empty() {
        assert!(render_sidecar_text_block(&[]).is_none());
    }

    /// Agents land in `agent_id` sort order regardless of input order, and
    /// two renders of the same input are byte-identical (the
    /// `--refresh-subagents` no-op re-capture path depends on this).
    #[test]
    fn render_sidecar_text_block_orders_by_agent_id_and_is_deterministic() {
        let agents = vec![
            ("z-agent".to_string(), "raw-z".to_string()),
            ("a-agent".to_string(), "raw-a".to_string()),
            ("m-agent".to_string(), "raw-m".to_string()),
        ];
        let once = render_sidecar_text_block(&agents).unwrap();
        let twice = render_sidecar_text_block(&agents).unwrap();
        assert_eq!(once, twice);

        let a_pos = once.find("a-agent").unwrap();
        let m_pos = once.find("m-agent").unwrap();
        let z_pos = once.find("z-agent").unwrap();
        assert!(a_pos < m_pos, "{once}");
        assert!(m_pos < z_pos, "{once}");

        // R14 — the block now opens with the un-hide companion <style> (a
        // deep link to `?raw=1#kb-session-sidecar-text` must actually reveal
        // the hidden section), THEN the section itself.
        assert!(
            once.starts_with(&sidecar_unhide_style_tag(SIDECAR_TEXT_BLOCK_ID)),
            "{once}"
        );
        assert!(
            once.contains(&format!(r#"<section id="{SIDECAR_TEXT_BLOCK_ID}" hidden>"#)),
            "{once}"
        );
        assert!(once.ends_with("</section>"));
        assert!(
            once.contains(r#"data-kb-sidecar-agent="a-agent""#),
            "{once}"
        );
        assert!(once.contains("<summary>a-agent</summary>"), "{once}");
        assert!(once.contains("<pre>raw-a</pre>"), "{once}");
    }

    #[test]
    fn transcript_size_verdict_proceeds_at_and_under_cap() {
        assert_eq!(
            transcript_size_verdict(100, 100, false),
            TranscriptSizeVerdict::Proceed
        );
        assert_eq!(
            transcript_size_verdict(99, 100, false),
            TranscriptSizeVerdict::Proceed
        );
    }

    #[test]
    fn transcript_size_verdict_refuses_over_cap_without_override() {
        assert_eq!(
            transcript_size_verdict(101, 100, false),
            TranscriptSizeVerdict::Refuse
        );
    }

    #[test]
    fn transcript_size_verdict_allow_oversized_overrides_the_refusal() {
        assert_eq!(
            transcript_size_verdict(101, 100, true),
            TranscriptSizeVerdict::Proceed
        );
    }

    #[test]
    fn sidecar_head_tail_split_is_sixty_forty_and_sums_to_budget() {
        assert_eq!(sidecar_head_tail_split(100), (60, 40));
        assert_eq!(sidecar_head_tail_split(8), (4, 4));
        assert_eq!(sidecar_head_tail_split(0), (0, 0));
        assert_eq!(sidecar_head_tail_split(1), (0, 1));
        for budget in [0usize, 1, 7, 8, 1_000_003] {
            let (head, tail) = sidecar_head_tail_split(budget);
            assert_eq!(head + tail, budget);
        }
    }

    /// Pure length-only math — no MB-sized fixtures. Four agents each
    /// wanting the full per-agent cap exactly exhaust the total cap; a
    /// fifth gets nothing.
    #[test]
    fn sidecar_agent_budgets_exhausts_the_total_cap_across_agents() {
        let lens = vec![3_000_000usize; 5];
        let budgets = sidecar_agent_budgets(&lens);
        assert_eq!(
            budgets,
            vec![
                SIDECAR_TEXT_AGENT_CAP_BYTES,
                SIDECAR_TEXT_AGENT_CAP_BYTES,
                SIDECAR_TEXT_AGENT_CAP_BYTES,
                SIDECAR_TEXT_AGENT_CAP_BYTES,
                0,
            ]
        );
        assert_eq!(budgets.iter().sum::<usize>(), SIDECAR_TEXT_TOTAL_CAP_BYTES);
    }

    /// An agent under its own per-agent cap leaves the UNUSED headroom for
    /// agents later in sort order — the total cap is charged by actual
    /// usage, not the assigned budget.
    #[test]
    fn sidecar_agent_budgets_reuses_unused_headroom_for_later_agents() {
        let half_cap = SIDECAR_TEXT_AGENT_CAP_BYTES / 2;
        let lens = vec![half_cap, half_cap, half_cap, half_cap, 5_000_000];
        let budgets = sidecar_agent_budgets(&lens);
        // Charging by ASSIGNED budget (not actual usage) would already have
        // exhausted the total after 4 agents (4 * AGENT_CAP ==
        // TOTAL_CAP), leaving the 5th with 0 — it must NOT be starved.
        assert_eq!(budgets[4], SIDECAR_TEXT_AGENT_CAP_BYTES);
    }

    #[test]
    fn sidecar_text_truncates_false_when_no_agents() {
        assert!(!sidecar_text_truncates(&[]));
    }

    #[test]
    fn sidecar_text_truncates_false_under_budget() {
        let agents = vec![
            ("a1".to_string(), "small".to_string()),
            ("a2".to_string(), "also small".to_string()),
        ];
        assert!(!sidecar_text_truncates(&agents));
    }

    #[test]
    fn sidecar_text_truncates_true_when_one_agent_exceeds_its_cap() {
        let agents = vec![(
            "a1".to_string(),
            "x".repeat(SIDECAR_TEXT_AGENT_CAP_BYTES + 1),
        )];
        assert!(sidecar_text_truncates(&agents));
    }

    /// The pure math must agree with the render fn on the SAME input: an
    /// over-cap agent both flips the flag AND leaves the marker in the
    /// rendered block, and the two never disagree since both walk
    /// `sidecar_agent_budgets` the same way.
    #[test]
    fn sidecar_text_truncates_agrees_with_render_sidecar_text_block() {
        let agents = vec![(
            "a1".to_string(),
            "x".repeat(SIDECAR_TEXT_AGENT_CAP_BYTES + 4096),
        )];
        assert!(sidecar_text_truncates(&agents));
        let rendered = render_sidecar_text_block(&agents).unwrap();
        assert!(rendered.contains("kb-sidecar-text: truncated"));
    }

    /// Truncate-THEN-escape: an entity character (`&`) sitting exactly at
    /// either cut edge lands fully inside the kept slice (never split by an
    /// escape-then-truncate bug), and the marker reports the exact dropped
    /// byte count.
    #[test]
    fn truncate_and_escape_agent_raw_splits_head_tail_and_escapes_after_truncating() {
        let raw = format!("AAA&{}&BBB", "X".repeat(13));
        assert_eq!(raw.len(), 21);

        let out = truncate_and_escape_agent_raw(&raw, 8);
        let expected = "AAA&amp;\n[kb-sidecar-text: truncated 13 bytes]\n&amp;BBB".to_string();
        assert_eq!(out, expected);
    }

    #[test]
    fn truncate_and_escape_agent_raw_under_budget_is_untruncated() {
        let raw = "small & <raw>";
        let out = truncate_and_escape_agent_raw(raw, 1024);
        assert_eq!(out, "small &amp; &lt;raw&gt;");
        assert!(!out.contains("kb-sidecar-text: truncated"));
    }

    #[test]
    fn extract_sidecar_text_block_absent_returns_empty() {
        let base = wrapper("sid-sidecar-empty", "hello\n");
        assert!(extract_sidecar_text_block(&base).is_empty());
    }

    #[test]
    fn extract_sidecar_text_block_round_trips_render() {
        let agents = vec![
            ("a1".to_string(), "line one\nline two\n".to_string()),
            ("a2".to_string(), "raw & <two>".to_string()),
        ];
        let block = render_sidecar_text_block(&agents).unwrap();
        let base = wrapper("sid-sidecar-rt", "hello\n");
        let with_block = base.replacen("</body>", &format!("{block}\n</body>"), 1);
        assert_eq!(extract_sidecar_text_block(&with_block), agents);
    }

    #[test]
    fn replace_sidecar_text_block_is_idempotent_and_updates_on_change() {
        let sid = "sid-sidecar-replace-1";
        let base = wrapper(sid, "hello\n");
        let agents_v1 = vec![("a1".to_string(), "raw-one".to_string())];
        let block_v1 = render_sidecar_text_block(&agents_v1);

        let once = replace_sidecar_text_block(&base, block_v1.clone());
        let twice = replace_sidecar_text_block(&once, block_v1);
        assert_eq!(once, twice, "unchanged inputs must byte-identically no-op");
        // R14 — the id substring now appears TWICE (the un-hide <style>
        // selector, then the <section id="…">), not once; the idempotency
        // property under test is unaffected.
        assert_eq!(once.matches(SIDECAR_TEXT_BLOCK_ID).count(), 2);

        let agents_v2 = vec![
            ("a1".to_string(), "raw-one".to_string()),
            ("a2".to_string(), "raw-two".to_string()),
        ];
        let block_v2 = render_sidecar_text_block(&agents_v2);
        let changed = replace_sidecar_text_block(&once, block_v2);
        assert_ne!(once, changed);
        assert_eq!(changed.matches(SIDECAR_TEXT_BLOCK_ID).count(), 2);
        assert_eq!(extract_sidecar_text_block(&changed), agents_v2);

        let removed = replace_sidecar_text_block(&changed, None);
        assert!(extract_sidecar_text_block(&removed).is_empty());
        assert!(!removed.contains(SIDECAR_TEXT_BLOCK_ID));
        assert_eq!(
            recover_jsonl_from_capture(&base),
            recover_jsonl_from_capture(&removed)
        );
    }

    #[test]
    fn replace_sidecar_text_block_inserts_after_an_existing_subagents_block() {
        let sid = "sid-sidecar-replace-2";
        let base = wrapper(sid, "hello\n");
        let digests = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 1,
            tool_calls: 1,
            errors: 0,
        }];
        let with_subagents = replace_subagents_block(&base, &digests);

        let sidecar_agents = vec![("a1".to_string(), "raw-a1".to_string())];
        let block = render_sidecar_text_block(&sidecar_agents);
        let rewritten = replace_sidecar_text_block(&with_subagents, block);

        let subagents_pos = rewritten.find(SUBAGENTS_BLOCK_ID).unwrap();
        let sidecar_pos = rewritten.find(SIDECAR_TEXT_BLOCK_ID).unwrap();
        assert!(subagents_pos < sidecar_pos, "{rewritten}");
        assert_eq!(
            recover_jsonl_from_capture(&base),
            recover_jsonl_from_capture(&rewritten)
        );
    }

    #[test]
    fn replace_sidecar_text_block_falls_back_to_before_body_without_a_subagents_block() {
        let sid = "sid-sidecar-replace-3";
        let base = wrapper(sid, "hello\n");
        let sidecar_agents = vec![("a1".to_string(), "raw-a1".to_string())];
        let block = render_sidecar_text_block(&sidecar_agents);
        let rewritten = replace_sidecar_text_block(&base, block);

        assert!(extract_subagents_block(&rewritten).is_none());
        assert_eq!(extract_sidecar_text_block(&rewritten), sidecar_agents);
        let body_pos = rewritten.find("</body>").unwrap();
        let sidecar_pos = rewritten.find(SIDECAR_TEXT_BLOCK_ID).unwrap();
        assert!(sidecar_pos < body_pos, "{rewritten}");
    }

    /// The full envelope — main `<pre>` + commits + subagents digest +
    /// sidecar-text — still recovers the MAIN jsonl byte-identically, and
    /// the session-digest/parse paths (which only ever look at the `<pre>`)
    /// are unaffected by the new tail block's presence.
    #[test]
    fn full_envelope_with_every_tail_block_still_round_trips_the_main_pre() {
        let sid = "sid-sidecar-roundtrip";
        let base = wrapper(sid, "hello world\n");

        let commits = vec![CapturedCommit {
            kind: "commit".to_string(),
            sha: Some("abc1234".to_string()),
            resolved: true,
            ..Default::default()
        }];
        let with_commits = base.replacen(
            "</pre>\n",
            &format!("</pre>\n{}\n", render_commits_block(&commits)),
            1,
        );

        let digests = vec![SubagentDigest {
            agent_id: "a1".to_string(),
            files: vec![],
            tokens: 5,
            tool_calls: 1,
            errors: 0,
        }];
        let with_subagents = replace_subagents_block(&with_commits, &digests);

        let sidecar_agents = vec![("a1".to_string(), "raw jsonl for a1\n".to_string())];
        let block = render_sidecar_text_block(&sidecar_agents);
        let full = replace_sidecar_text_block(&with_subagents, block);

        assert_eq!(
            recover_jsonl_from_capture(&base),
            recover_jsonl_from_capture(&full)
        );
        assert_eq!(extract_commits_block(&full).unwrap(), commits);
        assert_eq!(extract_subagents_block(&full).unwrap().agents, digests);
        assert_eq!(extract_sidecar_text_block(&full), sidecar_agents);

        let filename = "session-20260524T100000Z-sid-sidecar-roundtrip.html";
        let parse_without = parse_session_html_full(&with_subagents, filename, 0);
        let parse_with = parse_session_html_full(&full, filename, 0);
        assert_eq!(session_digest(&parse_without), session_digest(&parse_with));
    }

    // --- W0.6 amendment: `code` field cap ------------------------------------

    /// Pins the constant's value — the whole sizing arithmetic in its doc
    /// comment depends on this exact number.
    #[test]
    fn session_code_field_cap_bytes_is_32_kib() {
        assert_eq!(SESSION_CODE_FIELD_CAP_BYTES, 32 * 1024);
    }

    #[test]
    fn truncate_code_field_under_budget_is_byte_identical() {
        let code = "small code field, well under any cap";
        assert_eq!(truncate_code_field(code), code);
        assert_eq!(truncate_code_field_to(code, code.len()), code);
    }

    /// The core safety property the Arrow-overflow fix depends on: the
    /// output byte length NEVER exceeds the budget, for a range of budgets
    /// (including ones well under [`SESSION_CODE_FIELD_CAP_BYTES`]) and
    /// input sizes. Unlike the sidecar-text scheme (whose marker overshoot
    /// the budget only approximately — see
    /// [`SIDECAR_TEXT_TOTAL_CAP_BYTES`]), this MUST be a hard ceiling: it's
    /// what makes the corpus-wide sum argument in
    /// [`SESSION_CODE_FIELD_CAP_BYTES`]'s doc comment actually hold.
    #[test]
    fn truncate_code_field_to_never_exceeds_budget() {
        for budget in [128usize, 512, 4096, SESSION_CODE_FIELD_CAP_BYTES] {
            for extra in [1usize, 100, 10_000_000] {
                let code = "y".repeat(budget + extra);
                let out = truncate_code_field_to(&code, budget);
                assert!(
                    out.len() <= budget,
                    "budget={budget} extra={extra}: out.len()={} > budget",
                    out.len()
                );
            }
        }
    }

    /// Because the output of an over-budget truncation is proven `<=`
    /// budget (see the test above), re-truncating that output must hit the
    /// under-budget early-return and come back byte-identical — a true
    /// fixpoint after one pass, not just "close enough" determinism.
    #[test]
    fn truncate_code_field_is_idempotent_after_first_pass() {
        let budget = 200;
        let code = "z".repeat(50_000);
        let once = truncate_code_field_to(&code, budget);
        assert!(once.len() <= budget);
        let twice = truncate_code_field_to(&once, budget);
        assert_eq!(
            once, twice,
            "re-truncating an already-capped code field must be a no-op"
        );
    }

    #[test]
    fn truncate_code_field_to_is_deterministic() {
        let code = "abc".repeat(5000);
        let a = truncate_code_field_to(&code, 300);
        let b = truncate_code_field_to(&code, 300);
        assert_eq!(a, b);
    }

    /// Keeps the HEAD and TAIL content (a marker token planted at each end
    /// survives) while the MIDDLE is dropped and replaced by the marker
    /// line reporting the exact byte count removed.
    #[test]
    fn truncate_code_field_to_keeps_head_and_tail_drops_middle() {
        let head_token = "HEAD-TOKEN";
        let tail_token = "TAIL-TOKEN";
        let code = format!("{head_token}{}{tail_token}", "m".repeat(10_000));
        let out = truncate_code_field_to(&code, 500);
        assert!(out.starts_with(head_token), "{out}");
        assert!(out.ends_with(tail_token), "{out}");
        assert!(out.contains("[kb-code: truncated"), "{out}");
        assert!(out.len() < code.len());
    }

    /// UTF-8 char-boundary safety: multi-byte characters sitting right at
    /// the computed head/tail cut points must not be split (which would
    /// panic on the `&code[..head_end]` / `&code[tail_start..]` slices —
    /// a passing test at all proves no panic; this also confirms the kept
    /// halves are still valid, complete characters).
    #[test]
    fn truncate_code_field_to_is_utf8_char_boundary_safe() {
        // 3-byte-wide characters throughout, including right where the
        // 60/40 head/tail split would otherwise land mid-character. If
        // `floor_char_boundary`/`ceil_char_boundary` ever mis-snapped, the
        // `&code[..head_end]` / `&code[tail_start..]` slices below would
        // panic outright — reaching the assertions already proves
        // boundary-safety.
        let code = "日".repeat(2000);
        let out = truncate_code_field_to(&code, 137);
        assert!(out.contains("[kb-code: truncated"));
        // The marker text is pure ASCII; '日' is not. Filtering out ASCII
        // isolates the kept transcript content and confirms it's composed
        // of whole '日' characters only — no stray replacement bytes or
        // partial multi-byte sequences from a mid-character cut.
        let non_ascii: Vec<char> = out.chars().filter(|c| !c.is_ascii()).collect();
        assert!(!non_ascii.is_empty());
        assert!(non_ascii.iter().all(|&c| c == '日'), "{non_ascii:?}");
    }

    /// No entity-escape step: `code` (parser output) is already
    /// HTML-unescaped plain text and is never re-serialized into HTML, so
    /// literal `&`/`<`/`>` pass through the cap untouched (contrast with
    /// [`truncate_and_escape_agent_raw`], whose sidecar scheme escapes
    /// after truncating because that text DOES get spliced back into a
    /// `<pre>` block).
    #[test]
    fn truncate_code_field_does_not_escape_entities() {
        let code = "plain & <text> \"here\"";
        assert_eq!(truncate_code_field(code), code);
    }

    /// Regression test for the actual bug: simulate a corpus of
    /// [`SESSION_CODE_FIELD_CAP_BYTES`]'s own doc-comment reference point —
    /// 32,768 (2^15) session rows, each with a pathologically large raw
    /// `code` field (far exceeding the cap, modelling an uncapped
    /// multi-session-length transcript+sidecar) — and prove the CAPPED
    /// corpus-wide sum lands exactly where the doc comment claims (1 GiB,
    /// half the `i32::MAX` Arrow ceiling), nowhere near the overflow point.
    /// Pure Rust sum-and-assert, no lance/Arrow involved — this is the same
    /// arithmetic `interleave_bytes` would perform when merging every row's
    /// `code` value into one batch.
    #[test]
    fn code_field_cap_bounds_corpus_wide_sum_at_projected_scale() {
        let rows = 32_768usize;
        // One large synthetic value, reused (cheap) for every row — every
        // row is a worst case, matching the doc comment's "every row
        // simultaneously at the cap" scenario.
        let oversized = "w".repeat(10 * 1024 * 1024); // 10 MiB raw, uncapped
        let capped_len = truncate_code_field(&oversized).len();
        assert!(capped_len <= SESSION_CODE_FIELD_CAP_BYTES);

        // Worst-case bound — every row simultaneously AT the cap — is the
        // exact arithmetic the constant's doc comment states.
        let worst_case_total = (SESSION_CODE_FIELD_CAP_BYTES as u64) * (rows as u64);
        assert_eq!(
            worst_case_total,
            1024 * 1024 * 1024,
            "32,768 rows at the 32 KiB cap must sum to exactly 1 GiB in the worst case"
        );
        assert!(
            worst_case_total < i32::MAX as u64,
            "corpus-wide code sum ({worst_case_total}) must stay under the Arrow i32::Offset ceiling"
        );
        // Exactly half the ceiling (2^30 vs. i32::MAX's 2^31 - 1): the
        // corpus would have to DOUBLE AGAIN (to 65,536 rows, 2^16, ~71x
        // today's 916-row corpus) before the worst-case sum reaches the
        // ceiling at all.
        assert_eq!(worst_case_total * 2, (i32::MAX as u64) + 1);

        // The REAL simulated per-row output (which pays a small, fixed
        // marker overhead below the cap) can only be <= the worst case.
        let actual_total = (capped_len as u64) * (rows as u64);
        assert!(actual_total <= worst_case_total);
    }

    /// Same idea at today's REAL corpus size (916 rows, the live sessions
    /// kb this bug was reproduced against) with a realistic MIX of row
    /// sizes (most sessions small, a few pathologically large) — the sum
    /// stays a rounding error against the ceiling.
    #[test]
    fn code_field_cap_bounds_corpus_wide_sum_at_current_corpus_size() {
        let rows = 916usize;
        let mut total: u64 = 0;
        for i in 0..rows {
            // Every 10th row is huge (models a long agentic session); the
            // rest are small — either way each is passed through the cap.
            let raw = if i % 10 == 0 {
                "v".repeat(5 * 1024 * 1024)
            } else {
                "v".repeat(2048)
            };
            total += truncate_code_field(&raw).len() as u64;
        }
        assert!(
            total < 64 * 1024 * 1024,
            "916-row corpus-wide code sum ({total} bytes) should be tens of MB, nowhere near the 2.147 GB ceiling"
        );
    }
}
