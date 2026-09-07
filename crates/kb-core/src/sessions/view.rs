//! `session-view/1` — the ONE session interpretation engine (sessions-rethink
//! W1, memo R1). A pure, deterministic pass over a decoded transcript JSONL
//! (plus the envelope's additive tail blocks) that turns the flat event log
//! into a semantic tree: merged turns, paired tool calls, threaded task
//! lifecycles, an outline, a minimap feed, and a closure/outcome — the single
//! source every presenter (this wave: the HTML renderer; later waves: `kb
//! sessions read` and `GET /api/sessions/{sid}/view`) walks, so interpretation
//! rules exist ONCE instead of drifting between a CLI implementation and an
//! HTML one.
//!
//! Sibling of [`super::replay`] (`session-replay/1`) — same discipline:
//! transcript order preserved, no clock, no I/O, no LLM, golden-testable.
//! Where `replay` answers "what happened, beat by beat", `view` answers "what
//! does this conversation actually READ like" — richer, turn-shaped, with
//! results folded into their calls and synthetic wrapper envelopes
//! interpreted rather than shown as literal escaped prose.
//!
//! ## The join pass (memo R1 / reader.md Proposal 1)
//!
//! Four joins, all performed in ONE forward pass over the transcript
//! ([`ViewCarry::ingest_line`], driven by both [`session_view`] and
//! [`view_append`] — see "Incremental construction" below):
//!
//! 1. **requestId merge** — consecutive main-thread assistant records sharing
//!    a non-null `requestId` merge into ONE [`Turn`], including across
//!    intervening user records that are PURE tool_result carriers (the
//!    pairing join below consumes those without closing the turn).
//! 2. **tool_use ↔ tool_result pairing** — a `tool_use` block registers its
//!    id in a small pending-calls table (precedent: `parse_session_activity`'s
//!    `pending_git` map); a later tool_result with a matching `tool_use_id`
//!    folds into the call's [`Item::ToolCall`] at the CALL site. A user record
//!    that carried ONLY tool_results never becomes a [`Turn`] — `Role::Human`
//!    is reserved for turns a person actually authored. **Scope decision**
//!    (documented, not a silent gap): pairing is scoped to the turn that is
//!    still OPEN when the result arrives. In every fixture and every real
//!    transcript this codebase has seen, a tool_result is the very next
//!    record after its call (often the intervening carrier that keeps the
//!    turn open in the first place), so this is not a practical limitation —
//!    but it IS what makes `view_full(doc) ≡ fold(view_append)` provable: an
//!    already-EMITTED turn can never be retroactively mutated by a
//!    later-arriving chunk in incremental mode, so full-build mode must not
//!    do so either, or the two constructors would disagree by construction.
//! 3. **task lifecycle threading** — a `BTreeMap<String, TaskEntity>` fed by
//!    `TaskCreate`/`TaskUpdate`/`TaskOutput`/`Workflow` tool calls (+ their
//!    results) and `<task-notification>` wrapper payloads, keyed by whatever
//!    task id that tool's own shape carries (`taskId` / `task_id` on the
//!    input, or a `Task ID: <token>` / trailing `task <token>` pattern
//!    recovered from the result text — see [`extract_task_id`]).
//! 4. **sidechain grouping** — consecutive `isSidechain:true` turns fold into
//!    one [`SideLane`], computed as a derived pass over the final turn list
//!    (not carried in [`ViewCarry`] — a run can only be known complete once
//!    the NEXT non-sidechain turn is seen, and [`view_finish`] already forces
//!    that decision for every other kind of buffering).
//!
//! ## The interpretation catalog (reader.md Proposal 2, R14-corrected)
//!
//! [`super::UserTextClass`]/[`super::classify_user_text`] (shared with
//! [`super::parse_session_activity`]'s first-prompt fallback — R14) decides
//! what a `role:"user"` text actually is: a real prompt, a harness-synthetic
//! turn, or one of the [`super::WrapperKind`] envelopes. Each wrapper kind
//! gets its own [`Item`]: `<command-…>` → [`Item::Command`]; a following
//! `<local-command-stdout>` folds into the immediately-preceding Command as
//! its `stdout` (bounded one-turn lookahead — see
//! [`ViewCarry::pending_command`]); `<local-command-caveat>` is suppressed
//! entirely (never becomes a turn); `<task-notification>` joins the task
//! registry; `<system-reminder>` becomes [`Item::SystemReminder`], EXCEPT a
//! kb-recall memory-injection payload, which becomes [`Item::MemoryInjection`].
//!
//! ## Incremental construction (memo R15/LF-4)
//!
//! [`ViewCarry`] + [`view_append`] are a SECOND constructor over the exact
//! same per-line ingest code [`session_view`] itself calls
//! ([`ViewCarry::ingest_line`]) — built now, alongside the engine, not bolted
//! on later. `session_view(jsonl, …)` is literally `view_append` fed the
//! whole document in one chunk, followed by [`view_finish`]; the equivalence
//! `view_full(doc) ≡ fold(view_append over any chunking) + view_finish`
//! holds BY CONSTRUCTION for the turn stream (see the golden test
//! `equivalence_golden_across_chunk_sizes`), which is the point: one code
//! path, so a live-tail consumer (a future wave) and the full render can never
//! disagree about what a given line means.
//!
//! ## Tolerance floor
//!
//! A JSONL line that fails to parse becomes an [`Item::Raw`] rather than
//! aborting (mirrors `session_render`'s existing `Parsed::Raw` discipline);
//! [`session_view`] never panics on adversarial input (`garbage_input_still_
//! renders_something`).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::Serialize;
use serde_json::Value;

use super::{
    active_secs, classify_user_text, closing_text_from_prose_candidates, extract_commits_block,
    extract_subagents_block, floor_char_boundary, truncate_chars, CapturedCommit, SubagentsBlock,
    UserTextClass, WrapperKind,
};
use crate::review;
use crate::timeparse::parse_iso_utc;

/// Grammar tag for the wire shape this module produces — mirrors
/// [`super::replay::REPLAY_GRAMMAR`]'s convention. Bump the suffix (never the
/// meaning of an existing field) on an incompatible change.
pub const VIEW_GRAMMAR: &str = "session-view/1";

/// CT-A3 — the machine-readable recall-injection marker
/// `plugins/kb-memory/hooks/kb-recall.sh` appends right after each recalled
/// hit's human-readable line: `<!--kb-recall/1 kb=<kb-name> id=<hex12>[
/// pos=<n>]-->`. [`parse_recall_marker`] prefers it over
/// [`parse_recall_item`]'s free-text grammar, which stays the permanent
/// fallback (older captures, a hand-edited transcript). `ingest_attachment`
/// folds a marker line into its preceding hit exactly like a `↳` summary
/// continuation, so it never becomes a standalone `items[]` entry.
///
/// MR1 (SL6) — the prefix deliberately stops at the GRAMMAR NAME, before
/// `kb=`: the body is an unordered whitespace-separated `key=value` bag
/// (see [`parse_recall_marker`]), so pinning a key order here would make
/// the FOLD — which decides whether a marker becomes its own `items[]`
/// entry and therefore whether the ledger census double-counts — reject a
/// forward-compatible marker it should have absorbed.
const RECALL_MARKER_PREFIX: &str = "<!--kb-recall/1 ";
const RECALL_MARKER_SUFFIX: &str = "-->";

/// MR1 — the inclusive range a `pos=<n>` pair must land in to be believed.
/// The recall hook injects at most a handful of hits (`--limit 5` today);
/// a value outside this range is a mangled marker, and the honest answer
/// is "this hit's rank is unknown" (`pos: None`) rather than a number the
/// ledger would then display. It never invalidates the marker: `kb` and
/// `id` are what the row is FOR.
const RECALL_MARKER_POS_RANGE: std::ops::RangeInclusive<u32> = 1..=99;

/// Caps + tunables the engine reads. All fields have documented defaults so a
/// future wire route can expose a subset without changing the engine's
/// contract; nothing consumes non-default values yet (W1 is kb-core-only —
/// the wire projection lands in W2), but the shape is fixed now so W2/W4
/// don't have to redesign it.
#[derive(Debug, Clone)]
pub struct ViewOptions {
    /// Cap (chars) on a tool call's rendered input/result preview before it
    /// folds into a [`InputView::Summary`]/large-result marker.
    pub max_preview_chars: usize,
    /// Cap (chars) on a headline (tool name line, command chip, etc).
    pub max_headline_chars: usize,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            max_preview_chars: 2 * 1024,
            max_headline_chars: 90,
        }
    }
}

/// The envelope's additive tail blocks, already extracted by the caller (the
/// same [`extract_commits_block`]/[`extract_subagents_block`] the HTML
/// renderer used to read directly) — kept as a caller-supplied input rather
/// than re-parsed here so the engine stays a pure function of bytes it's
/// actually given, and so a caller that already has these (the render path)
/// doesn't re-scan the document.
#[derive(Debug, Clone, Default)]
pub struct TailBlocks {
    pub commits: Vec<CapturedCommit>,
    pub subagents: Option<SubagentsBlock>,
}

impl TailBlocks {
    /// Extract both blocks from a capture's full HTML in one call — the
    /// convenience constructor most callers want.
    pub fn from_html(html: &str) -> Self {
        Self {
            commits: extract_commits_block(html).unwrap_or_default(),
            subagents: extract_subagents_block(html),
        }
    }
}

// ─── the IR ─────────────────────────────────────────────────────────────

/// The full interpreted view of one session capture. Pure output of
/// [`session_view`] — deterministic given `(jsonl, tail, opts)`.
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    pub grammar: String,
    pub header: ViewHeader,
    pub turns: Vec<Turn>,
    /// Consecutive-sidechain-run groupings over `turns`, derived (each
    /// [`SideLane`] references its member turns by id — the flat `turns`
    /// list stays the single addressable source, so `?turn=`/outline/minimap
    /// never have to know about lanes).
    pub side_lanes: Vec<SideLane>,
    pub outline: Vec<OutlineRow>,
    pub tasks_final: Vec<TaskBoardRow>,
    pub subagents: Vec<SubagentSummary>,
    pub minimap: Vec<MinimapPoint>,
    pub stats: ViewStats,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize)]
pub struct ViewHeader {
    pub title: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub span_secs: Option<i64>,
    pub active_secs: i64,
    pub turns_human: u32,
    pub turns_assistant: u32,
    pub tool_calls: u32,
    pub error_count: u32,
    pub tokens: TokenLine,
    /// The first real typed prompt — the "asked:" peer of `outcome` (memo R3
    /// — the header carries both quotes side by side).
    pub opening: Option<Anchor>,
    pub outcome: Option<Outcome>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenLine {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl TokenLine {
    /// R6/P11 — the lead figure: what the model actually attended to
    /// (uncached input + everything served from cache), relabelled from the
    /// raw `input_tokens` figure the old header showed.
    pub fn effective_input(&self) -> u64 {
        self.input.saturating_add(self.cache_read)
    }
}

// `ts(rename = "ViewAnchor")`: `kb_core::review::Anchor` already exports as
// `Anchor.ts` — a bare export here would silently overwrite that unrelated
// binding (ts-rs names the file after the Rust type, not the module path).
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "ViewAnchor")
)]
#[derive(Debug, Clone, Serialize)]
pub struct Anchor {
    pub text: String,
    pub turn_id: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub text: String,
    pub turn_id: Option<String>,
    pub commits: Vec<CommitRef>,
    pub stop_reason: Option<String>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct CommitRef {
    pub kind: String,
    pub sha: Option<String>,
    pub subject: Option<String>,
    pub resolved: bool,
}

impl From<&CapturedCommit> for CommitRef {
    fn from(c: &CapturedCommit) -> Self {
        Self {
            kind: c.kind.clone(),
            sha: c.sha.clone(),
            subject: c.subject.clone(),
            resolved: c.resolved,
        }
    }
}

/// R2 — outline rows carry BOTH the render-order ordinal and the stable id,
/// so `?turn=N` (resolved via this list) and a stable `#t-<uuid12>` deep link
/// are both first-class. Real user prompts only (wrapper-skipping, R14) — a
/// Command/TaskNotification/SystemReminder-only turn never appears here.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct OutlineRow {
    pub n: u32,
    pub id: String,
    pub ts: Option<String>,
    pub preview: String,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct TaskBoardRow {
    pub id: String,
    pub subject: String,
    pub status: String,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct SubagentSummary {
    pub agent_id: String,
    pub files: usize,
    pub tokens: u64,
    pub tool_calls: u32,
    pub errors: u32,
    pub truncated: bool,
}

/// A consecutive run of sidechain turns (reader.md Proposal 4's "in-flow"
/// lane) — `turn_ids` in render order, so the presenter can locate + collapse
/// them without re-deriving the grouping.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct SideLane {
    pub agent: Option<String>,
    pub turn_ids: Vec<String>,
    pub item_count: usize,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MinimapKind {
    Human,
    Error,
    Commit,
    TaskDone,
    Sidechain,
}

/// One notable point on the activity minimap (reader.md Proposal 8) —
/// `pos_1000` is `(ordinal * 1000) / total_turns` computed with INTEGER math
/// only, so the feed (and any SVG built from it) is byte-deterministic.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct MinimapPoint {
    pub ordinal: u32,
    pub pos_1000: u32,
    pub kind: MinimapKind,
}

/// The honesty layer (reader.md Proposal 1's "P11-adjacent trust"): what the
/// parse pass actually saw, independent of what it chose to render.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, Serialize)]
pub struct ViewStats {
    pub events: u32,
    pub unparsed: u32,
    pub thinking_empty: u32,
    pub requests_merged: u32,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Human,
    Assistant,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    /// `"t-"` + the first 12 hex chars (dashes stripped) of the turn's
    /// SEED record's own `uuid` — the seed is the FIRST record in a merged
    /// group. Deterministic-hash fallback ([`fallback_turn_id`]) when no
    /// record in the group carries a `uuid` (rare — synthetic fixtures /
    /// very old transcripts).
    pub id: String,
    pub ordinal: u32,
    pub role: Role,
    pub ts: Option<String>,
    pub sidechain: bool,
    pub agent_id: Option<String>,
    pub items: Vec<Item>,
    /// L1/F1 — 0-based indices into the DECODED transcript\'s lines that produced
    /// this turn, in order (populated during the join pass — every record folded
    /// into the turn contributes its line index). For CLI use: fetch GET
    /// `/api/sessions/{sid}/raw` (text/plain decoded JSONL) and slice.
    pub raw_lines: Vec<u32>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolClass {
    File,
    Shell,
    Delegate,
    Net,
    Plan,
    Mcp,
    Other,
}

impl ToolClass {
    fn of(name: &str) -> Self {
        if name.starts_with("mcp__") {
            return ToolClass::Mcp;
        }
        match name {
            "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" | "NotebookRead" => {
                ToolClass::File
            }
            "Bash" | "Grep" | "Glob" | "LS" => ToolClass::Shell,
            "Agent" | "Task" | "Skill" => ToolClass::Delegate,
            "WebFetch" | "WebSearch" => ToolClass::Net,
            "TodoWrite" | "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskOutput" => {
                ToolClass::Plan
            }
            _ => ToolClass::Other,
        }
    }
}

/// A tool call's input, folded per reader.md Proposal 6: small payloads keep
/// their pretty form; anything over [`ViewOptions::max_preview_chars`] folds
/// to a key/byte-count summary rather than a wall of JSON.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum InputView {
    Small { pretty: String },
    Summary { keys: Vec<String>, bytes: usize },
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
pub struct ResultView {
    /// A short interpreted or truncated preview line.
    pub preview: String,
    pub is_error: bool,
    /// The full pretty-printed result JSON/text, for the per-card raw
    /// toggle — always re-derivable, never itself truncated silently (a
    /// huge body is the presenter's problem, not dropped here).
    pub raw: String,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "item")]
pub enum Item {
    Prose {
        text: String,
    },
    Thinking {
        empty: bool,
        len: u32,
        /// The full text — `None` when `empty` (the hygiene-bundle glyph
        /// path needs only the count; keeping `None` there avoids storing a
        /// redundant empty string on the ~100% of thinking blocks that ARE
        /// empty on Claude Code transcripts). `Some(text)` when non-empty,
        /// so the presenter can still show a preview + full body exactly as
        /// before — dropping this would regress "never worse than today"
        /// for the harnesses (Grok Build) whose thinking blocks are real.
        text: Option<String>,
    },
    ToolCall {
        name: String,
        kind: ToolClass,
        headline: String,
        input_view: InputView,
        result: Option<ResultView>,
        unpaired: bool,
        /// The verbatim `file_path`/`notebook_path` for a file-op tool
        /// (`Read`/`Write`/`Edit`/`NotebookEdit`/`NotebookRead`) — kept
        /// alongside `headline` (which may already show it, truncated) so
        /// the presenter can resolve it against the corpus mount table and
        /// render a click-to-open link, exactly as the pre-IR renderer did.
        /// `None` for every other tool (Bash args are deliberately never
        /// linkified — false-positive risk).
        path_for_link: Option<String>,
    },
    Command {
        name: String,
        args: Option<String>,
        stdout: Option<String>,
    },
    TaskEvent {
        id: String,
        subject: String,
        transition: String,
    },
    WorkflowCard {
        name: Option<String>,
        description: Option<String>,
        phases: Vec<String>,
        task_id: Option<String>,
    },
    KbCommand {
        verb: String,
        args: String,
        result_view: Option<String>,
    },
    MemoryInjection {
        items: Vec<String>,
    },
    SystemReminder {
        preview: String,
    },
    Decision {
        prompt: String,
        answer: Option<String>,
    },
    ModeChange {
        mode: String,
    },
    TimeGap {
        secs: i64,
    },
    Raw {
        reason: String,
    },
}

// ─── the incremental carry ─────────────────────────────────────────────

/// One pending tool call awaiting its result — an index into whichever item
/// list currently owns it. Scoped to the OPEN turn only (see the module docs'
/// "tool_use ↔ tool_result pairing" section for why).
///
/// Ordered FIFO (`order`) for the eviction cap; `map` for O(1) id lookup.
/// Insertion order is preserved for the cap's oldest-first drop; membership
/// is the map alone.
#[derive(Debug, Clone, Default)]
struct PendingCalls {
    order: VecDeque<String>,
    map: HashMap<String, usize>,
}

#[derive(Debug, Clone)]
struct TaskEntity {
    subject: String,
    status: String,
}

#[derive(Debug, Clone)]
struct OpenTurn {
    id: String,
    ordinal: u32,
    role: Role,
    request_id: Option<String>,
    ts: Option<String>,
    sidechain: bool,
    agent_id: Option<String>,
    items: Vec<Item>,
    pending_calls: PendingCalls,
    raw_lines: Vec<u32>,
}

impl OpenTurn {
    fn close(self) -> Turn {
        Turn {
            id: self.id,
            ordinal: self.ordinal,
            role: self.role,
            ts: self.ts,
            sidechain: self.sidechain,
            agent_id: self.agent_id,
            items: self.items,
            raw_lines: self.raw_lines,
        }
    }
}

/// One unit [`view_append`] hands back to its caller. Only `TurnClosed` was
/// exercised by the equivalence golden in W1; W7/LF-4 is the first real
/// consumer of `TaskUpdated` (the live tail panel's task-board delta,
/// emitted without waiting for the owning turn to close) and of the wire
/// serialization below (the `/live` route + `kb sessions read --follow
/// --json`'s NDJSON — LF-3b/LF-6).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum ViewEvent {
    TurnClosed(Turn),
    TaskUpdated(TaskBoardRow),
}

/// The incremental engine's resumable state (memo R15/LF-4). Carries
/// everything [`session_view`] itself accumulates across the WHOLE document —
/// [`session_view`] is literally `view_append(ViewCarry::default(), whole_doc)`
/// followed by [`view_finish`], so the two constructors cannot drift.
#[derive(Debug, Clone, Default)]
pub struct ViewCarry {
    open: Option<OpenTurn>,
    /// A fully-closed Command turn held back for exactly one more record, in
    /// case the NEXT record is its `<local-command-stdout>` companion (which
    /// must fold into it rather than becoming its own turn).
    pending_command: Option<Turn>,
    /// Interstitial markers (time gaps, mode changes, small system chips)
    /// collected between turns, prepended to the next turn's items — or
    /// flushed as a standalone turn at [`view_finish`] if none follows.
    pending_pre_items: Vec<Item>,
    last_ts: Option<String>,
    last_mode: Option<String>,
    task_registry: BTreeMap<String, TaskEntity>,
    ordinal: u32,
    /// Bounded FIFO of seen record uuids (eviction order).
    seen_uuids: VecDeque<String>,
    /// O(1) membership companion for [`seen_uuids`] — same elements, no order.
    seen_uuid_set: HashSet<String>,
    /// L1/F1 — 0-based line number counter for the decoded transcript.
    line_number: u32,
    // header accumulation
    title: Option<String>,
    cwd: Option<String>,
    git_branch: Option<String>,
    model: Option<String>,
    harness: Option<String>,
    started_at: Option<String>,
    ended_at: Option<String>,
    tokens: TokenLine,
    tool_calls: u32,
    error_count: u32,
    stop_reason: Option<String>,
    event_times: Vec<i64>,
    // stats
    events: u32,
    unparsed: u32,
    thinking_empty: u32,
    requests_merged: u32,
}

/// Bound on the pending tool-call table (memo R15/LF-4: "open_calls cap
/// 256 FIFO"). A single merged turn issuing this many un-paired calls before
/// any result arrives is not a shape any real transcript has shown; on
/// overflow the OLDEST pending call is evicted (it simply stays `unpaired`
/// when — if ever — its result shows up), bounding memory without ever
/// panicking.
const PENDING_CALLS_CAP: usize = 256;
/// Bound on the seen-uuid dedup ring (memo R15/LF-4) — protects a future
/// live-tail consumer against a re-delivered line double-counting.
const SEEN_UUID_CAP: usize = 512;

impl ViewCarry {
    pub fn new() -> Self {
        Self::default()
    }

    /// W7/LF-3b — the honesty counters accumulated so far (cumulative since
    /// this carry's `ViewCarry::default()`, i.e. since a follow session's
    /// last bootstrap — NOT reset per `view_append` call). The `/live`
    /// route surfaces `stats().unparsed` (+ the tailer's own
    /// `fragment_dropped`, which the carry has no visibility into) as its
    /// wire `parse_failures`, mirroring `ViewStats`'s role on the full
    /// `SessionView`.
    pub fn stats(&self) -> ViewStats {
        ViewStats {
            events: self.events,
            unparsed: self.unparsed,
            thinking_empty: self.thinking_empty,
            requests_merged: self.requests_merged,
        }
    }

    /// Feed `lines` (zero or more COMPLETE JSONL lines — partial-line
    /// buffering across chunks is not this wave's contract, see the build
    /// report) into the carry, returning every [`ViewEvent`] the new lines
    /// completed. The still-open turn (if any) is intentionally NOT flushed —
    /// call [`view_finish`] once no more chunks are coming.
    pub fn ingest(&mut self, lines: &str) -> Vec<ViewEvent> {
        let mut out = Vec::new();
        for line in lines.lines() {
            let line_idx = self.line_number;
            self.ingest_line(line, &mut out);
            if let Some(open) = &mut self.open {
                open.raw_lines.push(line_idx);
            }
            self.line_number += 1;
        }
        out
    }

    fn emit_open_pre_items(&mut self, kind: Role, ts: Option<&str>) -> &mut OpenTurn {
        if self.open.is_none() {
            self.ordinal += 1;
            let id = fallback_turn_id(self.ordinal, ts.unwrap_or(""));
            self.open = Some(OpenTurn {
                id,
                ordinal: self.ordinal,
                role: kind,
                request_id: None,
                ts: ts.map(str::to_string),
                sidechain: false,
                agent_id: None,
                items: std::mem::take(&mut self.pending_pre_items),
                pending_calls: PendingCalls::default(),
                raw_lines: Vec::new(),
            });
        }
        self.open.as_mut().expect("just set")
    }

    /// Close whatever turn is open (if any), pushing unresolved pending
    /// calls back into their items as `unpaired: true` (already their
    /// default — nothing to do but drop the pending table) and returning the
    /// finished [`Turn`], ghost-eliminated (an all-empty turn yields `None`
    /// and its ordinal is reclaimed).
    fn close_open(&mut self) -> Option<Turn> {
        let open = self.open.take()?;
        if open.items.is_empty() {
            // Ghost elimination — reclaim the ordinal so numbering stays
            // dense over what actually renders.
            self.ordinal = self.ordinal.saturating_sub(1);
            return None;
        }
        Some(open.close())
    }

    /// Flush [`pending_command`] unconditionally (it did not get a stdout
    /// companion) into `out`.
    fn flush_pending_command(&mut self, out: &mut Vec<ViewEvent>) {
        if let Some(t) = self.pending_command.take() {
            out.push(ViewEvent::TurnClosed(t));
        }
    }

    fn ingest_line(&mut self, line: &str, out: &mut Vec<ViewEvent>) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        self.events += 1;
        let v: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => {
                self.unparsed += 1;
                self.flush_pending_command(out);
                let ts = self.last_ts.clone();
                let turn = self.emit_open_pre_items(Role::Assistant, ts.as_deref());
                turn.items.push(Item::Raw {
                    reason: "unparseable JSON line".to_string(),
                });
                return;
            }
        };
        self.ingest_value(&v, out);
    }

    fn ingest_value(&mut self, v: &Value, out: &mut Vec<ViewEvent>) {
        if let Some(uuid) = v.get("uuid").and_then(|x| x.as_str()) {
            let uuid = uuid.to_string();
            if self.seen_uuid_set.contains(&uuid) {
                return;
            }
            self.seen_uuid_set.insert(uuid.clone());
            self.seen_uuids.push_back(uuid);
            if self.seen_uuids.len() > SEEN_UUID_CAP {
                if let Some(old) = self.seen_uuids.pop_front() {
                    self.seen_uuid_set.remove(&old);
                }
            }
        }

        self.absorb_scalars(v);

        let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let role = v
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(|x| x.as_str());

        match role {
            Some("assistant") => self.ingest_assistant(v, out),
            Some("user") => self.ingest_user(v, out),
            _ => match ty {
                "permission-mode" => self.ingest_mode(v),
                "system" => {
                    if let Some(r) = v.get("stopReason").and_then(|x| x.as_str()) {
                        self.stop_reason = Some(r.to_string());
                    }
                }
                "attachment" => self.ingest_attachment(v),
                _ => {} // ai-title/cwd/etc already absorbed; mode/queue/snapshot: no item
            },
        }
    }

    fn absorb_scalars(&mut self, v: &Value) {
        if self.title.is_none() {
            if let Some(t) = v.get("aiTitle").and_then(|x| x.as_str()) {
                if !t.trim().is_empty() {
                    self.title = Some(t.trim().to_string());
                }
            }
        } else if let Some(t) = v.get("aiTitle").and_then(|x| x.as_str()) {
            // LAST wins, mirrors parse_session_activity.
            if !t.trim().is_empty() {
                self.title = Some(t.trim().to_string());
            }
        }
        if let Some(c) = v.get("cwd").and_then(|x| x.as_str()) {
            if !c.is_empty() {
                self.cwd = Some(c.to_string());
            }
        }
        if self.git_branch.is_none() {
            if let Some(b) = v.get("gitBranch").and_then(|x| x.as_str()) {
                if !b.is_empty() {
                    self.git_branch = Some(b.to_string());
                }
            }
        }
        if self.harness.is_none() && v.get("type").and_then(|x| x.as_str()) == Some("adapter-meta")
        {
            if let Some(h) = v.get("harness").and_then(|x| x.as_str()) {
                if !h.trim().is_empty() {
                    self.harness = Some(h.trim().to_string());
                }
            }
        }
        if let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) {
            if let Some(unix) = parse_iso_utc(ts) {
                self.event_times.push(unix);
            }
            if self.started_at.is_none() {
                self.started_at = Some(ts.to_string());
            }
            self.ended_at = Some(ts.to_string());
            self.last_ts = Some(ts.to_string());
        }
    }

    fn ingest_mode(&mut self, v: &Value) {
        let mode = v
            .get("permissionMode")
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_string();
        if self.last_mode.as_deref() != Some(mode.as_str()) {
            self.last_mode = Some(mode.clone());
            self.pending_pre_items.push(Item::ModeChange { mode });
        }
    }

    fn ingest_attachment(&mut self, v: &Value) {
        let Some(att) = v.get("attachment") else {
            return;
        };
        let ty = att.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let preview = match ty {
            "hook_additional_context" => {
                let body = attachment_text(att, "content").unwrap_or_default();
                if body.trim_start().starts_with("Relevant memories from kb") {
                    // Each hit is one `- <title> ...` line; kb-recall.sh may
                    // append a `↳ <summary>` continuation line right after it
                    // (plugins/kb-memory/hooks/kb-recall.sh). Fold any such
                    // line into the PRECEDING hit (joined with `\n`) rather
                    // than letting it become its own standalone item — a
                    // flat per-line walk would otherwise turn one recalled
                    // hit into two `items[]` entries, double-counting it for
                    // any consumer that treats `items.len()` as "hits
                    // injected" (the injection-ledger census, W1.1).
                    let mut items: Vec<String> = Vec::new();
                    for line in body
                        .lines()
                        .skip(1)
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                    {
                        if let Some(summary) = line.strip_prefix('↳') {
                            if let Some(last) = items.last_mut() {
                                last.push('\n');
                                last.push_str(summary.trim());
                                continue;
                            }
                            // Orphan continuation with no preceding hit
                            // (shouldn't happen from kb-recall.sh's own
                            // output) — fall through and keep it as its own
                            // item rather than silently dropping data.
                        } else if line.starts_with(RECALL_MARKER_PREFIX)
                            && line.ends_with(RECALL_MARKER_SUFFIX)
                        {
                            // CT-A3 machine marker — folds into the
                            // preceding hit exactly like the `↳` summary
                            // above, so `items.len()` stays one entry PER
                            // HIT (the injection-ledger census, W1.1) even
                            // though the marker is a physically separate
                            // line.
                            if let Some(last) = items.last_mut() {
                                last.push('\n');
                                last.push_str(line);
                                continue;
                            }
                            // Orphan marker with no preceding hit — keep as
                            // its own item rather than silently dropping
                            // data (mirrors the `↳` orphan path above).
                        }
                        items.push(line.trim_start_matches('-').trim().to_string());
                    }
                    if !items.is_empty() {
                        self.pending_pre_items.push(Item::MemoryInjection { items });
                        return;
                    }
                }
                let name = att
                    .get("hookName")
                    .and_then(|x| x.as_str())
                    .unwrap_or("hook");
                format!("hook: {name}")
            }
            "edited_text_file" => {
                let f = att.get("filename").and_then(|x| x.as_str()).unwrap_or("");
                format!("edit: {f}")
            }
            "queued_command" => {
                let p = att.get("prompt").and_then(|x| x.as_str()).unwrap_or("");
                format!("queued: {}", truncate_chars(p, 80))
            }
            "task_reminder"
            | "skill_listing"
            | "auto_mode"
            | "plan_mode"
            | "plan_mode_exit"
            | "deferred_tools_delta"
            | "mcp_instructions_delta"
            | "hook_success" => return,
            other => format!("attachment: {other}"),
        };
        self.pending_pre_items
            .push(Item::SystemReminder { preview });
    }

    fn ingest_assistant(&mut self, v: &Value, out: &mut Vec<ViewEvent>) {
        self.flush_pending_command(out);
        let request_id = v
            .get("requestId")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let is_sidechain = v
            .get("isSidechain")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let ts = v.get("timestamp").and_then(|x| x.as_str());
        let uuid = v.get("uuid").and_then(|x| x.as_str());

        let merges = matches!(
            (&self.open, &request_id),
            (Some(o), Some(rid))
                if o.role == Role::Assistant && o.request_id.as_deref() == Some(rid.as_str())
        );

        if !merges {
            if let Some(closed) = self.close_open() {
                out.push(ViewEvent::TurnClosed(closed));
            }
            self.ordinal += 1;
            let id = uuid
                .map(turn_id_from_uuid)
                .unwrap_or_else(|| fallback_turn_id(self.ordinal, ts.unwrap_or("")));
            self.open = Some(OpenTurn {
                id,
                ordinal: self.ordinal,
                role: Role::Assistant,
                request_id: request_id.clone(),
                ts: ts.map(str::to_string),
                sidechain: is_sidechain,
                agent_id: None,
                items: std::mem::take(&mut self.pending_pre_items),
                pending_calls: PendingCalls::default(),
                raw_lines: Vec::new(),
            });
        } else {
            self.requests_merged += 1;
        }

        // Model + usage aggregation.
        if let Some(m) = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|x| x.as_str())
        {
            if !m.is_empty() {
                self.model = Some(m.to_string());
            }
        }
        if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
            self.tokens.input = self
                .tokens
                .input
                .saturating_add(u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0));
            self.tokens.output = self
                .tokens
                .output
                .saturating_add(u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0));
            self.tokens.cache_write = self.tokens.cache_write.saturating_add(
                u.get("cache_creation_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            );
            self.tokens.cache_read = self.tokens.cache_read.saturating_add(
                u.get("cache_read_input_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            );
        }

        let Some(blocks) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            return;
        };
        for b in blocks {
            self.ingest_assistant_block(b);
        }
    }

    fn ingest_assistant_block(&mut self, b: &Value) {
        let ty = b.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match ty {
            "text" => {
                let text = b.get("text").and_then(|x| x.as_str()).unwrap_or("");
                if !text.trim().is_empty() {
                    self.push_open_item(Item::Prose {
                        text: text.trim().to_string(),
                    });
                }
            }
            "thinking" => {
                let t = b.get("thinking").and_then(|x| x.as_str()).unwrap_or("");
                let empty = t.trim().is_empty();
                if empty {
                    self.thinking_empty += 1;
                }
                self.push_open_item(Item::Thinking {
                    empty,
                    len: t.chars().count() as u32,
                    text: (!empty).then(|| t.trim().to_string()),
                });
            }
            "tool_use" => self.ingest_tool_use(b),
            "image" => self.push_open_item(Item::Prose {
                text: "[image attachment]".to_string(),
            }),
            _ => {}
        }
    }

    fn ingest_tool_use(&mut self, b: &Value) {
        let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let id = b
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let input = b.get("input").cloned().unwrap_or(Value::Null);
        self.tool_calls += 1;

        // Task lifecycle short-circuits: these tools become their own Item
        // kinds instead of a generic ToolCall (P9/P4).
        match name {
            "TaskUpdate" => {
                let task_id = task_id_field(&input).unwrap_or_default();
                let status = input
                    .get("status")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let subject = self
                    .task_registry
                    .get(&task_id)
                    .map(|e| e.subject.clone())
                    .unwrap_or_default();
                self.task_registry
                    .entry(task_id.clone())
                    .and_modify(|e| e.status = status.clone())
                    .or_insert_with(|| TaskEntity {
                        subject: subject.clone(),
                        status: status.clone(),
                    });
                self.push_open_item(Item::TaskEvent {
                    id: task_id,
                    subject,
                    transition: status,
                });
                return;
            }
            "TaskCreate" => {
                let subject = input
                    .get("subject")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if let Some(open) = self.open.as_mut() {
                    open.items.push(Item::ToolCall {
                        name: name.to_string(),
                        kind: ToolClass::of(name),
                        headline: truncate_chars(&subject, 90),
                        input_view: input_view(&input, ViewOptions::default().max_preview_chars),
                        result: None,
                        unpaired: true,
                        path_for_link: None,
                    });
                    let idx = open.items.len() - 1;
                    register_pending(&mut open.pending_calls, id.clone(), idx);
                    // Stash the subject where the result handler can find it
                    // by reusing the TaskCreate headline (already on the
                    // item) once the result resolves the real task id.
                }
                return;
            }
            "Workflow" => {
                let (wf_name, description, phases) = parse_workflow_meta(&input);
                self.push_open_item(Item::WorkflowCard {
                    name: wf_name,
                    description,
                    phases,
                    task_id: None,
                });
                if let Some(open) = self.open.as_mut() {
                    let idx = open.items.len() - 1;
                    register_pending(&mut open.pending_calls, id.clone(), idx);
                }
                return;
            }
            _ => {}
        }

        if name == "Bash" {
            if let Some(cmd) = input.get("command").and_then(|x| x.as_str()) {
                if let Some((verb, args)) = kb_cli_invocation(cmd) {
                    self.push_open_item(Item::KbCommand {
                        verb,
                        args,
                        result_view: None,
                    });
                    if let Some(open) = self.open.as_mut() {
                        let idx = open.items.len() - 1;
                        register_pending(&mut open.pending_calls, id.clone(), idx);
                    }
                    return;
                }
            }
        }

        let headline = tool_use_headline(name, &input);
        let path_for_link = if matches!(
            name,
            "Read" | "Write" | "Edit" | "NotebookEdit" | "NotebookRead"
        ) {
            input
                .get("file_path")
                .or_else(|| input.get("notebook_path"))
                .and_then(|x| x.as_str())
                .map(str::to_string)
        } else {
            None
        };
        self.push_open_item(Item::ToolCall {
            name: name.to_string(),
            kind: ToolClass::of(name),
            headline,
            input_view: input_view(&input, ViewOptions::default().max_preview_chars),
            result: None,
            unpaired: true,
            path_for_link,
        });
        if let Some(open) = self.open.as_mut() {
            let idx = open.items.len() - 1;
            register_pending(&mut open.pending_calls, id, idx);
        }
    }

    fn ingest_user(&mut self, v: &Value, out: &mut Vec<ViewEvent>) {
        let content = v.get("message").and_then(|m| m.get("content"));
        let blocks: Vec<&Value> = match content {
            Some(Value::Array(arr)) => arr.iter().collect(),
            _ => Vec::new(),
        };
        let has_results = blocks
            .iter()
            .any(|b| b.get("type").and_then(|x| x.as_str()) == Some("tool_result"));
        let text_blocks: Vec<&Value> = blocks
            .iter()
            .filter(|b| b.get("type").and_then(|x| x.as_str()) == Some("text"))
            .copied()
            .collect();

        // Fold every tool_result present into its call, wherever it lives.
        if has_results {
            let event_tul = v.get("toolUseResult");
            for b in &blocks {
                if b.get("type").and_then(|x| x.as_str()) != Some("tool_result") {
                    continue;
                }
                self.fold_tool_result(b, event_tul);
            }
        }

        // Pure result carrier (no real text content) — never becomes a Turn.
        let plain_string_content = matches!(content, Some(Value::String(_)));
        let real_text = if plain_string_content {
            content.and_then(|c| c.as_str()).map(str::to_string)
        } else if !text_blocks.is_empty() {
            Some(
                text_blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|x| x.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        } else {
            None
        };

        let Some(text) = real_text else {
            return; // pure tool_result carrier — results already folded above
        };
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        let is_meta = v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false);
        let is_sidechain = v
            .get("isSidechain")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        let prompt_source = v.get("promptSource").and_then(|x| x.as_str());
        let class = classify_user_text(text, is_meta, prompt_source);
        let ts = v.get("timestamp").and_then(|x| x.as_str());
        let uuid = v.get("uuid").and_then(|x| x.as_str());

        // Any real user record (even a suppressed wrapper) closes whatever
        // assistant turn was open — a human turn boundary.
        if let Some(closed) = self.close_open() {
            out.push(ViewEvent::TurnClosed(closed));
        }

        match class {
            UserTextClass::Meta => {
                // Synthetic/meta turns are suppressed entirely (never a
                // Human turn) — but a LocalCommandCaveat specifically still
                // routes through the wrapper arm below when it's ALSO
                // is_meta (the common real shape); a bare isMeta with no
                // recognised wrapper is dropped outright.
                if let Some(WrapperKind::LocalCommandCaveat) = classify_wrapper_only(text) {
                    self.flush_pending_command(out);
                }
                // else: nothing rendered, ordinal untouched (never opened).
            }
            UserTextClass::Wrapper(WrapperKind::LocalCommandCaveat) => {
                self.flush_pending_command(out);
                // suppressed — no item, no turn.
            }
            UserTextClass::Wrapper(WrapperKind::LocalCommandStdout) => {
                let stdout = strip_stdout_wrapper(text);
                if let Some(cmd_turn) = self.pending_command.as_mut() {
                    if let Some(Item::Command { stdout: s, .. }) = cmd_turn.items.last_mut() {
                        *s = Some(stdout);
                    }
                    let done = self.pending_command.take().expect("checked Some above");
                    out.push(ViewEvent::TurnClosed(done));
                } else {
                    // No preceding command — standalone chip (never worse
                    // than dropping it).
                    self.ordinal += 1;
                    let id = uuid
                        .map(turn_id_from_uuid)
                        .unwrap_or_else(|| fallback_turn_id(self.ordinal, ts.unwrap_or("")));
                    out.push(ViewEvent::TurnClosed(Turn {
                        id,
                        ordinal: self.ordinal,
                        role: Role::Human,
                        ts: ts.map(str::to_string),
                        sidechain: is_sidechain,
                        agent_id: None,
                        items: vec![Item::Command {
                            name: "(stdout)".to_string(),
                            args: None,
                            stdout: Some(stdout),
                        }],
                        raw_lines: Vec::new(),
                    }));
                }
            }
            UserTextClass::Wrapper(WrapperKind::Command) => {
                self.flush_pending_command(out);
                let (name, args) = parse_command_wrapper(text);
                self.ordinal += 1;
                let id = uuid
                    .map(turn_id_from_uuid)
                    .unwrap_or_else(|| fallback_turn_id(self.ordinal, ts.unwrap_or("")));
                let mut items = std::mem::take(&mut self.pending_pre_items);
                items.push(Item::Command {
                    name,
                    args,
                    stdout: None,
                });
                self.pending_command = Some(Turn {
                    id,
                    ordinal: self.ordinal,
                    role: Role::Human,
                    ts: ts.map(str::to_string),
                    sidechain: is_sidechain,
                    agent_id: None,
                    items,
                    raw_lines: Vec::new(),
                });
            }
            UserTextClass::Wrapper(WrapperKind::TaskNotification) => {
                self.flush_pending_command(out);
                let (task_id, status, summary) = parse_task_notification(text);
                let task_id = task_id.unwrap_or_default();
                let subject = summary.clone().unwrap_or_else(|| {
                    self.task_registry
                        .get(&task_id)
                        .map(|e| e.subject.clone())
                        .unwrap_or_default()
                });
                let status_s = status.unwrap_or_default();
                self.task_registry
                    .entry(task_id.clone())
                    .and_modify(|e| {
                        e.status = status_s.clone();
                        if summary.is_some() {
                            e.subject = subject.clone();
                        }
                    })
                    .or_insert_with(|| TaskEntity {
                        subject: subject.clone(),
                        status: status_s.clone(),
                    });
                self.ordinal += 1;
                let id = uuid
                    .map(turn_id_from_uuid)
                    .unwrap_or_else(|| fallback_turn_id(self.ordinal, ts.unwrap_or("")));
                let mut items = std::mem::take(&mut self.pending_pre_items);
                items.push(Item::TaskEvent {
                    id: task_id,
                    subject,
                    transition: status_s,
                });
                out.push(ViewEvent::TurnClosed(Turn {
                    id,
                    ordinal: self.ordinal,
                    role: Role::Human,
                    ts: ts.map(str::to_string),
                    sidechain: is_sidechain,
                    agent_id: None,
                    items,
                    raw_lines: Vec::new(),
                }));
            }
            UserTextClass::Wrapper(WrapperKind::SystemReminder) => {
                self.flush_pending_command(out);
                let inner = strip_tag(text, "system-reminder").unwrap_or(text);
                self.ordinal += 1;
                let id = uuid
                    .map(turn_id_from_uuid)
                    .unwrap_or_else(|| fallback_turn_id(self.ordinal, ts.unwrap_or("")));
                let mut items = std::mem::take(&mut self.pending_pre_items);
                items.push(Item::SystemReminder {
                    preview: truncate_chars(inner.trim(), 200),
                });
                out.push(ViewEvent::TurnClosed(Turn {
                    id,
                    ordinal: self.ordinal,
                    role: Role::Human,
                    ts: ts.map(str::to_string),
                    sidechain: is_sidechain,
                    agent_id: None,
                    items,
                    raw_lines: Vec::new(),
                }));
            }
            UserTextClass::Wrapper(WrapperKind::LocalCommandOther | WrapperKind::BashStdio) => {
                self.flush_pending_command(out);
                self.ordinal += 1;
                let id = uuid
                    .map(turn_id_from_uuid)
                    .unwrap_or_else(|| fallback_turn_id(self.ordinal, ts.unwrap_or("")));
                let mut items = std::mem::take(&mut self.pending_pre_items);
                items.push(Item::Raw {
                    reason: "unrecognised wrapper envelope".to_string(),
                });
                out.push(ViewEvent::TurnClosed(Turn {
                    id,
                    ordinal: self.ordinal,
                    role: Role::Human,
                    ts: ts.map(str::to_string),
                    sidechain: is_sidechain,
                    agent_id: None,
                    items,
                    raw_lines: Vec::new(),
                }));
            }
            UserTextClass::Real => {
                self.flush_pending_command(out);
                self.ordinal += 1;
                let id = uuid
                    .map(turn_id_from_uuid)
                    .unwrap_or_else(|| fallback_turn_id(self.ordinal, ts.unwrap_or("")));
                let mut items = std::mem::take(&mut self.pending_pre_items);
                items.push(Item::Prose {
                    text: text.to_string(),
                });
                out.push(ViewEvent::TurnClosed(Turn {
                    id,
                    ordinal: self.ordinal,
                    role: Role::Human,
                    ts: ts.map(str::to_string),
                    sidechain: is_sidechain,
                    agent_id: None,
                    items,
                    raw_lines: Vec::new(),
                }));
            }
        }
    }

    /// Fold a `tool_result` block into its call, wherever it lives: the
    /// currently open turn's pending table, or (rare) the held-back
    /// `pending_command` turn — commands never issue tool calls though, so
    /// in practice this always resolves against `open`. `event_tul` is the
    /// EVENT-level `toolUseResult` (a sibling of `message`, not the block) —
    /// where AskUserQuestion's structured `answers` map lives.
    fn fold_tool_result(&mut self, b: &Value, event_tul: Option<&Value>) {
        let Some(id) = b.get("tool_use_id").and_then(|x| x.as_str()) else {
            return;
        };
        let is_error = b.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false);
        if is_error {
            self.error_count += 1;
        }
        let body = tool_result_text_view(b);
        let raw = serde_json::to_string_pretty(b.get("content").unwrap_or(&Value::Null))
            .unwrap_or_default();

        let Some(open) = self.open.as_mut() else {
            return;
        };
        let Some(idx) = find_and_remove(&mut open.pending_calls, id) else {
            return;
        };

        // Decision detection (AskUserQuestion answers / ExitPlanMode
        // approval / tool-use rejection / permission denial) — ported
        // verbatim from the pre-IR renderer's `render_structured_decision`/
        // `render_user_decision`, checked BEFORE generic result-folding on
        // EVERY tool_result (the original never gated this on tool name —
        // a permission denial can ride any tool's result). A structured or
        // prose match REPLACES the call item with one [`Item::Decision`]
        // PER question/answer pair (the closed `Item` enum holds a single
        // prompt/answer pair — a multi-question AskUserQuestion renders as
        // several small Decision items instead of one grouped `<dl>` card,
        // a deliberate, documented v2 rendering change, not a data loss).
        if let Some(pairs) = detect_decision_pairs(event_tul, &body) {
            if idx < open.items.len() {
                let decisions: Vec<Item> = pairs
                    .into_iter()
                    .map(|(prompt, answer)| Item::Decision { prompt, answer })
                    .collect();
                open.items.splice(idx..idx + 1, decisions);
            }
            return;
        }

        let Some(item) = open.items.get_mut(idx) else {
            return;
        };
        match item {
            Item::ToolCall {
                result, unpaired, ..
            } => {
                *unpaired = false;
                *result = Some(ResultView {
                    preview: truncate_chars(body.trim(), 140),
                    is_error,
                    raw,
                });
            }
            Item::WorkflowCard { task_id, .. } => {
                if let Some(tid) = extract_task_id(&body) {
                    *task_id = Some(tid.clone());
                    let subject_from_workflow = String::new();
                    self.task_registry.entry(tid).or_insert_with(|| TaskEntity {
                        subject: subject_from_workflow,
                        status: "launched".to_string(),
                    });
                }
            }
            Item::KbCommand {
                verb, result_view, ..
            } => {
                *result_view = Some(interpret_kb_output(verb, &body));
            }
            _ => {}
        }
        // TaskCreate is registered as a ToolCall above (falls into the first
        // arm); additionally resolve its subject into the task registry the
        // moment its id is known.
        if let Item::ToolCall { name, headline, .. } = &open.items[idx] {
            if name == "TaskCreate" {
                if let Some(tid) = extract_task_id(&body) {
                    self.task_registry.entry(tid).or_insert_with(|| TaskEntity {
                        subject: headline.clone(),
                        status: "created".to_string(),
                    });
                }
            }
        }
    }

    fn push_open_item(&mut self, item: Item) {
        let ts = self.last_ts.clone();
        let turn = self.emit_open_pre_items(Role::Assistant, ts.as_deref());
        turn.items.push(item);
    }
}

fn register_pending(pending: &mut PendingCalls, id: String, idx: usize) {
    // Re-register of the same id updates the item index in place and keeps
    // the original FIFO slot (matches "last write wins" for pairing while
    // preserving the cap's oldest-first eviction order).
    if pending.map.insert(id.clone(), idx).is_none() {
        pending.order.push_back(id);
    }
    while pending.map.len() > PENDING_CALLS_CAP {
        let Some(old) = pending.order.pop_front() else {
            break;
        };
        pending.map.remove(&old);
    }
}

fn find_and_remove(pending: &mut PendingCalls, id: &str) -> Option<usize> {
    let idx = pending.map.remove(id)?;
    // Drop the matching FIFO slot so `order` never retains orphans (a long
    // session of pair-and-forget would otherwise grow `order` unbounded).
    if let Some(pos) = pending.order.iter().position(|k| k == id) {
        pending.order.remove(pos);
    }
    Some(idx)
}

/// R2 — `"t-"` + first 12 hex chars (dashes stripped) of the record uuid.
///
/// `sessions::replay` reuses this verbatim to stamp each beat's best-effort
/// `turn` ref (R7/S6) — see that module's `ReplayBeat::turn` doc.
///
/// **`pub` since V73-K3** (widened from `pub(crate)`, no behaviour change):
/// kb-code's hunk↔turn join mints the SAME `t-<uuid12>` id so a match it
/// reports addresses the turn `kb sessions read` addresses. Two copies of a
/// three-line derivation would be two things to keep in step, and root
/// invariant #11 makes this id part of a cross-daemon contract rather than
/// an internal detail — so the derivation is exported rather than mirrored.
pub fn turn_id_from_uuid(uuid: &str) -> String {
    let hex: String = uuid.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    format!("t-{}", &hex[..hex.len().min(12)])
}

/// Deterministic fallback when no record in a merge group carries a `uuid` —
/// a SHA-256 prefix of `ordinal:ts` (the same "hash12" shape `ArtifactId`
/// uses elsewhere in kb-core), never `std::hash` (not cross-build-stable).
fn fallback_turn_id(ordinal: u32, ts: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("{ordinal}:{ts}").as_bytes());
    let digest = hasher.finalize();
    format!("t-{}", hex::encode(&digest[..6]))
}

/// Just the wrapper classification, ignoring `promptSource`/`isMeta` — used
/// only to detect "this isMeta text is ALSO a recognised caveat wrapper" in
/// [`ViewCarry::ingest_user`]'s `Meta` arm.
fn classify_wrapper_only(text: &str) -> Option<WrapperKind> {
    match classify_user_text(text, false, None) {
        UserTextClass::Wrapper(k) => Some(k),
        _ => None,
    }
}

fn strip_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

/// `<command-name>/cmd</command-name>\n<command-args>a b</command-args>` →
/// `(name, args)`. Tolerant: a missing args tag yields `None`.
fn parse_command_wrapper(text: &str) -> (String, Option<String>) {
    let name = strip_tag(text, "command-name")
        .map(str::to_string)
        .unwrap_or_else(|| "(command)".to_string());
    let args = strip_tag(text, "command-args")
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string);
    (name, args)
}

/// W6 (P12 tail — W4 builder report, "bare SGR remnant" polish). Command
/// stdout for every presenter is populated ONLY here (the ONE call site,
/// the `LocalCommandStdout` arm above), so the strip below is a single
/// change three consumers (`session_render.rs`, `kb sessions read`,
/// `GET /api/sessions/{sid}/view`) inherit for free.
fn strip_stdout_wrapper(text: &str) -> String {
    let unwrapped = strip_tag(text, "local-command-stdout").unwrap_or(text);
    strip_bare_sgr_remnants(unwrapped).into_owned()
}

/// Bare SGR (Select Graphic Rendition) remnants. The capture pipeline
/// already strips the ESC byte (`0x1B`) out of terminal output before it
/// reaches the transcript — but that leaves the REST of an ANSI escape
/// sequence behind as literal text: `\x1b[1m` becomes bare `[1m`,
/// `\x1b[22m` becomes `[22m`, a reset-then-color pair becomes `[0m[32m`, and
/// so on. This strips those remnants at RENDER-interpretation time (the raw
/// captured bytes on disk are untouched — invariant #11's byte-identical
/// `<pre>` — this only shapes what `Item::Command.stdout` carries).
///
/// Pattern (conservative, exactly the SGR parameter shape and nothing
/// looser): `[` + 1-3 digits + zero or more `;`+1-3-digits groups + a
/// literal `m`. This deliberately does NOT match ordinary prose brackets —
/// `[note]`, `[1]`, `[TODO]`, a markdown link's `[text]` — none of those end
/// in a bare `m` immediately after digits. The only plausible false
/// positive is command output that literally reads e.g. "elapsed [22m]"
/// (minutes, coincidentally SGR-shaped) — rare enough to accept for a
/// presentation-only cosmetic strip.
fn strip_bare_sgr_remnants(text: &str) -> std::borrow::Cow<'_, str> {
    bare_sgr_regex().replace_all(text, "")
}

fn bare_sgr_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\[\d{1,3}(?:;\d{1,3})*m").expect("valid bare-SGR-remnant regex")
    })
}

/// `<task-notification>\n<task-id>X</task-id>\n<tool-use-id>…</tool-use-id>\n
/// <status>Y</status>\n<summary>Z</summary>\n</task-notification>` → the
/// three fields, each tolerant of absence.
fn parse_task_notification(text: &str) -> (Option<String>, Option<String>, Option<String>) {
    let id = strip_tag(text, "task-id").map(str::to_string);
    let status = strip_tag(text, "status").map(str::to_string);
    let summary = strip_tag(text, "summary")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (id, status, summary)
}

/// Detect an explicit user-decision pattern in a tool_result — the
/// STRUCTURED AskUserQuestion `answers` map (preferred: immune to prose
/// wording flips) or, failing that, the four known prose shapes. `None` for
/// an ordinary tool result. Ported verbatim (string-for-string) from the
/// pre-IR `session_render::render_structured_decision`/`render_user_decision`.
fn detect_decision_pairs(
    event_tul: Option<&Value>,
    body: &str,
) -> Option<Vec<(String, Option<String>)>> {
    if let Some(tul) = event_tul {
        if let Some(answers) = tul.get("answers").and_then(|a| a.as_object()) {
            if !answers.is_empty() {
                let mut pairs: Vec<(String, Option<String>)> = Vec::new();
                if let Some(questions) = tul.get("questions").and_then(|q| q.as_array()) {
                    for q in questions {
                        if let Some(qt) = q.get("question").and_then(|x| x.as_str()) {
                            if let Some(a) = answers.get(qt) {
                                pairs.push((qt.to_string(), Some(answer_to_string(a))));
                            }
                        }
                    }
                }
                if pairs.is_empty() {
                    for (q, a) in answers {
                        pairs.push((q.clone(), Some(answer_to_string(a))));
                    }
                }
                if !pairs.is_empty() {
                    return Some(pairs);
                }
            }
        }
    }
    let trimmed = body.trim_start();
    let qa_rest = trimmed
        .strip_prefix("Your questions have been answered: ")
        .or_else(|| trimmed.strip_prefix("User has answered your questions: "));
    if let Some(rest) = qa_rest {
        let pairs = parse_qa_pairs(rest);
        if !pairs.is_empty() {
            return Some(pairs.into_iter().map(|(q, a)| (q, Some(a))).collect());
        }
    }
    if trimmed.starts_with("User has approved your plan") {
        return Some(vec![("plan approved".to_string(), None)]);
    }
    if trimmed.starts_with("The user doesn't want to proceed with this tool use") {
        return Some(vec![("tool use rejected".to_string(), None)]);
    }
    if let Some(rest) = trimmed.strip_prefix("Permission for this action was denied") {
        let reason = rest
            .split_once("Reason:")
            .map(|(_, s)| truncate_chars(s.trim(), 240));
        return Some(vec![("permission denied".to_string(), reason)]);
    }
    None
}

/// Stringify an AskUserQuestion answer value: a string verbatim, an array
/// comma-joined (multiSelect), else its JSON form. Mirrors
/// `sessions::answer_to_string`.
fn answer_to_string(a: &Value) -> String {
    match a {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    }
}

/// Tolerant parser for the `"Q"="A", "Q2"="A2"` tail of an AskUserQuestion
/// prose result — ported verbatim from `session_render::parse_qa_pairs`.
fn parse_qa_pairs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '"' {
            break;
        }
        i += 1;
        let q_start = i;
        while i < chars.len() && chars[i] != '"' {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let q: String = chars[q_start..i].iter().collect();
        i += 1;
        if i >= chars.len() || chars[i] != '=' {
            break;
        }
        i += 1;
        if i >= chars.len() || chars[i] != '"' {
            break;
        }
        i += 1;
        let a_start = i;
        while i < chars.len() && chars[i] != '"' {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let a: String = chars[a_start..i].iter().collect();
        i += 1;
        out.push((q, a));
        while i < chars.len() && (chars[i] == ',' || chars[i].is_whitespace()) {
            i += 1;
        }
    }
    out
}

/// `taskId` (TaskUpdate's own shape) then `task_id` (TaskOutput's) then `id`.
fn task_id_field(input: &Value) -> Option<String> {
    for key in ["taskId", "task_id", "id"] {
        if let Some(v) = input.get(key) {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
            if let Some(n) = v.as_i64() {
                return Some(n.to_string());
            }
        }
    }
    None
}

/// Best-effort task id recovery from a result/notification body: prefer the
/// explicit `Task ID: <token>` shape (Workflow launches); else the trailing
/// token after the LAST case-insensitive `"task "` occurrence (`"Created
/// task 1"` / `"Updated task 1"`), trimmed of trailing punctuation.
fn extract_task_id(text: &str) -> Option<String> {
    if let Some(idx) = text.find("Task ID:") {
        let tail = text[idx + "Task ID:".len()..].trim_start();
        let tok: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !tok.is_empty() {
            return Some(tok);
        }
    }
    let lower = text.to_ascii_lowercase();
    let idx = lower.rfind("task ")?;
    let tail = text[idx + "task ".len()..].trim_start();
    let tok: String = tail
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    (!tok.is_empty()).then_some(tok)
}

/// A bounded, deterministic scanner for a `Workflow` tool_use's `meta` object
/// literal (reader.md Proposal 4.3) — pulls `name:`/`description:` string
/// literals and a `phase`-shaped entry list from the input script text.
/// Never a full JS parse: on any ambiguity, fields fall back to `None`/empty
/// rather than guessing. Scan is capped at 64 KiB of input (arbitrary scripts
/// are attacker-adjacent, not attacker-controlled, but bounding the scan cost
/// is cheap insurance). Cap is snapped to a UTF-8 char boundary so a multi-byte
/// character straddling the cut never panics on the slice.
const WORKFLOW_SCAN_CAP_BYTES: usize = 64 * 1024;

fn parse_workflow_meta(input: &Value) -> (Option<String>, Option<String>, Vec<String>) {
    let script = input
        .get("script")
        .and_then(|x| x.as_str())
        .unwrap_or_default();
    let end = floor_char_boundary(script, script.len().min(WORKFLOW_SCAN_CAP_BYTES));
    let script = &script[..end];
    let name = scan_meta_field(script, "name");
    let description = scan_meta_field(script, "description");
    let phases = scan_phase_entries(script);
    (name, description, phases)
}

/// Find `<key>:` followed by a single/double-quoted string literal anywhere
/// in `script` (first hit wins) — a deliberately dumb, bounded scan, never a
/// real JS/TS parser.
fn scan_meta_field(script: &str, key: &str) -> Option<String> {
    let needle = format!("{key}:");
    let at = script.find(&needle)?;
    let rest = script[at + needle.len()..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '\'' && quote != '"' {
        return None;
    }
    let body = &rest[1..];
    let end = body.find(quote)?;
    let val = &body[..end];
    (!val.trim().is_empty()).then(|| val.trim().to_string())
}

/// Pull `phase:` string-literal entries in appearance order (best-effort,
/// deduped) — a `phases: ['recon','build']`-shaped array or repeated
/// `phase: 'x'` fields both work since this is a linear scan, not a
/// structural parse.
fn scan_phase_entries(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = script;
    while let Some(at) = rest.find("phase") {
        let after = &rest[at + "phase".len()..];
        let after = after.trim_start_matches([':', ' ', '\'', '"']);
        // walk back to include the opening quote we just trimmed, then scan
        // for the literal exactly like scan_meta_field does.
        let quoted = &rest[at + "phase".len()..];
        let quoted = quoted.trim_start();
        let quoted = quoted.strip_prefix(':').unwrap_or(quoted).trim_start();
        if let Some(quote) = quoted.chars().next().filter(|c| *c == '\'' || *c == '"') {
            let body = &quoted[1..];
            if let Some(end) = body.find(quote) {
                let val = body[..end].trim().to_string();
                if !val.is_empty() && !out.contains(&val) {
                    out.push(val);
                }
            }
        }
        rest = after;
        if out.len() > 32 {
            break; // sane upper bound — a workflow doesn't have 32 phases
        }
    }
    out
}

/// Generalises `sessions::kb_cli_query`'s env/`cd &&`-tolerant segment
/// scanner to EVERY `kb` verb (display purposes — the RA2 research-signal
/// exclusion of recall/remember/why/recollect binds only digest
/// classification, never display, per reader.md Proposal 5).
pub fn kb_cli_invocation(cmd: &str) -> Option<(String, String)> {
    for seg in cmd.split(['&', ';', '|', '\n']) {
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
        if verb.is_empty() || verb.starts_with('-') {
            continue;
        }
        return Some((
            verb.to_string(),
            parts.next().unwrap_or("").trim().to_string(),
        ));
    }
    None
}

/// Version-tolerant, line-oriented recognizers for a handful of `kb`
/// verbs' human-readable output (reader.md Proposal 5) — golden-tested
/// against the exact strings the synthetic kb-commands fixture carries.
/// Anything else (or a shape that doesn't match) falls through to the
/// generic preview — never worse than an opaque shell card.
fn interpret_kb_output(verb: &str, body: &str) -> String {
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    match verb {
        "recall" | "search" | "find" | "related" | "recollect" => {
            let hits = lines.iter().filter(|l| starts_with_ordinal(l)).count();
            if hits > 0 {
                format!(
                    "{hits} hit{} · {}",
                    if hits == 1 { "" } else { "s" },
                    truncate_chars(lines[0], 90)
                )
            } else {
                truncate_chars(body.trim(), 120)
            }
        }
        "remember" => {
            if let Some(l) = lines.iter().find(|l| l.starts_with("remembered ")) {
                truncate_chars(l, 120)
            } else {
                truncate_chars(body.trim(), 120)
            }
        }
        "why" => truncate_chars(body.trim(), 160),
        _ => truncate_chars(body.trim(), 120),
    }
}

fn starts_with_ordinal(line: &str) -> bool {
    let t = line.trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    !digits.is_empty() && t[digits.len()..].starts_with('.')
}

/// Build a short "first-arg preview" for the tool-use summary line — ported
/// verbatim from the pre-IR renderer (`session_render::tool_use_headline`) so
/// existing headline expectations don't drift.
fn tool_use_headline(name: &str, input: &Value) -> String {
    let key = match name {
        "Bash" => "command",
        "Read" | "Write" | "Edit" | "NotebookEdit" => "file_path",
        "Grep" => "pattern",
        "Glob" => "pattern",
        "WebFetch" | "WebSearch" => "url",
        "Agent" | "Task" => "description",
        "Skill" => "skill",
        "ScheduleWakeup" => "reason",
        _ => "",
    };
    if !key.is_empty() {
        if let Some(v) = input.get(key).and_then(|x| x.as_str()) {
            return truncate_chars(v, 90);
        }
    }
    if let Some(obj) = input.as_object() {
        for v in obj.values() {
            if let Some(s) = v.as_str() {
                return truncate_chars(s, 90);
            }
        }
    }
    String::new()
}

fn input_view(input: &Value, cap_chars: usize) -> InputView {
    let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
    if pretty.chars().count() > cap_chars {
        let keys: Vec<String> = input
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        InputView::Summary {
            keys,
            bytes: pretty.len(),
        }
    } else {
        InputView::Small { pretty }
    }
}

fn tool_result_text_view(b: &Value) -> String {
    match b.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|it| {
                it.get("text")
                    .and_then(|t| t.as_str())
                    .or_else(|| it.as_str())
                    .map(str::to_string)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
        None => String::new(),
    }
}

fn attachment_text(att: &Value, key: &str) -> Option<String> {
    match att.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) => Some(
            arr.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        v => Some(v.to_string()),
    }
}

// ─── the two constructors ───────────────────────────────────────────────

/// Feed `lines` into `carry`, returning every completed [`ViewEvent`] and the
/// updated carry. `lines` must be zero or more COMPLETE JSONL lines (a
/// documented W1 simplification — partial-line buffering across a byte-level
/// chunk boundary is deferred to whichever wave wires an actual byte-stream
/// live-tail; see the build report). The still-open turn is intentionally
/// held back — call [`view_finish`] once no more input is coming.
pub fn view_append(mut carry: ViewCarry, lines: &str) -> (Vec<ViewEvent>, ViewCarry) {
    let events = carry.ingest(lines);
    (events, carry)
}

/// Flush whatever [`view_append`] left open: the pending Command lookahead,
/// the open turn, and any leftover interstitial items with nowhere to attach
/// (rendered as one final small turn rather than silently dropped).
///
/// Takes `&mut ViewCarry` so the full-document constructor can finish
/// without cloning the whole carry just to keep reading header/stats fields.
pub fn view_finish(carry: &mut ViewCarry) -> Vec<ViewEvent> {
    let mut out = Vec::new();
    carry.flush_pending_command(&mut out);
    if let Some(closed) = carry.close_open() {
        out.push(ViewEvent::TurnClosed(closed));
    }
    if !carry.pending_pre_items.is_empty() {
        carry.ordinal += 1;
        let id = fallback_turn_id(carry.ordinal, carry.last_ts.as_deref().unwrap_or(""));
        out.push(ViewEvent::TurnClosed(Turn {
            id,
            ordinal: carry.ordinal,
            role: Role::Assistant,
            ts: carry.last_ts.clone(),
            sidechain: false,
            agent_id: None,
            items: std::mem::take(&mut carry.pending_pre_items),
            raw_lines: Vec::new(),
        }));
    }
    out
}

/// Bootstrap a carry from a byte WINDOW (e.g. the tail of a live transcript)
/// without emitting anything — best-effort priming (mode/task-registry state
/// built from the window may be incomplete when the window doesn't start at
/// the beginning of the file; documented, not a correctness claim for a
/// window that starts mid-turn).
pub fn view_bootstrap(bytes_window: &str) -> ViewCarry {
    let (_, carry) = view_append(ViewCarry::default(), bytes_window);
    carry
}

/// The full-document constructor: `session_view` IS `view_append` fed the
/// whole document in one call, followed by [`view_finish`] — see the module
/// docs' "Incremental construction" section for why this equivalence is
/// structural, not coincidental.
pub fn session_view(jsonl: &str, tail: &TailBlocks, _opts: &ViewOptions) -> SessionView {
    let (mut events, mut carry) = view_append(ViewCarry::default(), jsonl);
    events.extend(view_finish(&mut carry));

    let mut turns: Vec<Turn> = events
        .into_iter()
        .filter_map(|e| match e {
            ViewEvent::TurnClosed(t) => Some(t),
            ViewEvent::TaskUpdated(_) => None,
        })
        .collect();
    turns.sort_by_key(|t| t.ordinal);

    let side_lanes = build_side_lanes(&turns);
    let outline = build_outline(&turns);
    let minimap = build_minimap(&turns);
    let tasks_final = carry
        .task_registry
        .iter()
        .map(|(id, e)| TaskBoardRow {
            id: id.clone(),
            subject: e.subject.clone(),
            status: e.status.clone(),
        })
        .collect();
    let subagents = tail
        .subagents
        .as_ref()
        .map(|block| {
            block
                .agents
                .iter()
                .map(|a| SubagentSummary {
                    agent_id: a.agent_id.clone(),
                    files: a.files.len(),
                    tokens: a.tokens,
                    tool_calls: a.tool_calls,
                    errors: a.errors,
                    truncated: block.truncated,
                })
                .collect()
        })
        .unwrap_or_default();

    let opening = outline.first().map(|row| Anchor {
        text: outline_full_prose(&turns, &row.id).unwrap_or_else(|| row.preview.clone()),
        turn_id: Some(row.id.clone()),
    });
    let outcome = build_outcome(&turns, tail, carry.stop_reason.clone());

    let event_times = carry.event_times.clone();
    let active = active_secs(&event_times);
    let span_secs = match (&carry.started_at, &carry.ended_at) {
        (Some(a), Some(b)) => match (parse_iso_utc(a), parse_iso_utc(b)) {
            (Some(a), Some(b)) => Some((b - a).max(0)),
            _ => None,
        },
        _ => None,
    };

    let header = ViewHeader {
        title: carry.title.clone(),
        harness: carry
            .harness
            .clone()
            .unwrap_or_else(|| super::HARNESS_DEFAULT.to_string()),
        model: carry.model.clone(),
        cwd: carry.cwd.clone(),
        git_branch: carry.git_branch.clone(),
        started_at: carry.started_at.clone(),
        ended_at: carry.ended_at.clone(),
        span_secs,
        active_secs: active,
        turns_human: turns.iter().filter(|t| t.role == Role::Human).count() as u32,
        turns_assistant: turns.iter().filter(|t| t.role == Role::Assistant).count() as u32,
        tool_calls: carry.tool_calls,
        error_count: carry.error_count,
        tokens: carry.tokens.clone(),
        opening,
        outcome,
    };

    let stats = ViewStats {
        events: carry.events,
        unparsed: carry.unparsed,
        thinking_empty: carry.thinking_empty,
        requests_merged: carry.requests_merged,
    };

    SessionView {
        grammar: VIEW_GRAMMAR.to_string(),
        header,
        turns,
        side_lanes,
        outline,
        tasks_final,
        subagents,
        minimap,
        stats,
    }
}

/// Recover a turn's full (untruncated) leading Prose text by id — used for
/// [`ViewHeader::opening`], which wants more than the 60-char outline
/// preview.
fn outline_full_prose(turns: &[Turn], id: &str) -> Option<String> {
    let turn = turns.iter().find(|t| t.id == id)?;
    turn.items.iter().find_map(|i| match i {
        Item::Prose { text } => Some(text.clone()),
        _ => None,
    })
}

/// R2/R6 — real user prompts only. A Human turn's PRIMARY prose is the first
/// [`Item::Prose`] anywhere in its items — NOT strictly `items[0]`: an
/// interstitial chip (a `<system-reminder>`/memory-injection/mode-change
/// absorbed while no turn was open) can land ahead of the real prompt when
/// nothing intervened to drain `pending_pre_items` first (e.g. a recall-hook
/// injection immediately preceding the typed prompt it was injected for).
fn build_outline(turns: &[Turn]) -> Vec<OutlineRow> {
    turns
        .iter()
        .filter(|t| t.role == Role::Human)
        .filter_map(|t| {
            t.items.iter().find_map(|i| match i {
                Item::Prose { text } => Some(OutlineRow {
                    n: t.ordinal,
                    id: t.id.clone(),
                    ts: t.ts.clone(),
                    preview: truncate_chars(text, 60),
                }),
                _ => None,
            })
        })
        .collect()
}

fn build_side_lanes(turns: &[Turn]) -> Vec<SideLane> {
    let mut lanes = Vec::new();
    let mut i = 0;
    while i < turns.len() {
        if !turns[i].sidechain {
            i += 1;
            continue;
        }
        let agent = turns[i].agent_id.clone();
        let mut ids = Vec::new();
        let mut item_count = 0;
        let mut j = i;
        while j < turns.len() && turns[j].sidechain && turns[j].agent_id == agent {
            ids.push(turns[j].id.clone());
            item_count += turns[j].items.len();
            j += 1;
        }
        lanes.push(SideLane {
            agent,
            turn_ids: ids,
            item_count,
        });
        i = j;
    }
    lanes
}

fn build_minimap(turns: &[Turn]) -> Vec<MinimapPoint> {
    let total = turns.len().max(1) as u32;
    let mut points = Vec::new();
    for t in turns {
        let pos_1000 = (t.ordinal.saturating_sub(1) * 1000) / total;
        if t.role == Role::Human && t.items.iter().any(|i| matches!(i, Item::Prose { .. })) {
            points.push(MinimapPoint {
                ordinal: t.ordinal,
                pos_1000,
                kind: MinimapKind::Human,
            });
        }
        if t.sidechain {
            points.push(MinimapPoint {
                ordinal: t.ordinal,
                pos_1000,
                kind: MinimapKind::Sidechain,
            });
        }
        for item in &t.items {
            match item {
                Item::ToolCall {
                    result: Some(r), ..
                } if r.is_error => {
                    points.push(MinimapPoint {
                        ordinal: t.ordinal,
                        pos_1000,
                        kind: MinimapKind::Error,
                    });
                }
                Item::TaskEvent { transition, .. }
                    if transition == "completed" || transition == "success" =>
                {
                    points.push(MinimapPoint {
                        ordinal: t.ordinal,
                        pos_1000,
                        kind: MinimapKind::TaskDone,
                    });
                }
                _ => {}
            }
        }
    }
    points
}

/// R3 — the outcome, derived from the Assistant Prose items the view walk
/// already produced (substantial/any last-wins, matching
/// [`super::closing_assistant_text`]'s rule) plus the commits tail block.
/// No second full-JSONL re-parse — the walk's turns are the source.
fn build_outcome(
    turns: &[Turn],
    tail: &TailBlocks,
    stop_reason: Option<String>,
) -> Option<Outcome> {
    let text = closing_text_from_prose_candidates(
        turns
            .iter()
            .filter(|t| t.role == Role::Assistant && !t.sidechain)
            .flat_map(|t| t.items.iter())
            .filter_map(|i| match i {
                Item::Prose { text } => Some(text.as_str()),
                _ => None,
            }),
    )?;
    // Best-effort turn attribution: the last Assistant turn carrying ANY
    // Prose item. Not a strict text match against `text` (a merged turn may
    // join several original records' text blocks) — the footer renders
    // `text` verbatim regardless, this only supplies the anchor to jump to.
    let turn_id = turns
        .iter()
        .rev()
        .find(|t| {
            t.role == Role::Assistant && t.items.iter().any(|i| matches!(i, Item::Prose { .. }))
        })
        .map(|t| t.id.clone());
    Some(Outcome {
        text,
        turn_id,
        commits: tail.commits.iter().map(CommitRef::from).collect(),
        stop_reason,
    })
}

/// `N` or `A..B` → an inclusive `(min, max)` ordinal window. `A..B` is
/// order-independent (`7..5` ≡ `5..7`). `None` on empty or malformed input
/// (callers then serve the unwindowed section rather than erroring).
/// Shared by the CLI (`kb sessions read --turn`) and the server
/// (`?turns=`) so the two grammars cannot drift.
pub fn parse_turn_window(spec: &str) -> Option<(u32, u32)> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    if let Some((a, b)) = spec.split_once("..") {
        let a: u32 = a.trim().parse().ok()?;
        let b: u32 = b.trim().parse().ok()?;
        Some((a.min(b), a.max(b)))
    } else {
        let n: u32 = spec.parse().ok()?;
        Some((n, n))
    }
}

// ─── W6 — memory-session comment-anchor resolution (moonshots M2 / memo R2) ─
//
// The verified gap: the indexer's stale-anchor pass
// (`indexer::finish_indexed_doc`) resolves every open comment's anchor
// against the RAW capture bytes via `review::fuzzy_resolve_anchor_with` —
// fine for an ordinary artifact, but a capture's raw HTML is just
// `<h1>…</h1><pre>{escaped JSONL}</pre>` plus additive tail blocks (no
// `t-<uuid12>` ids, no `#ses-outcome` section, no readable prose — the
// escaped JSONL characters are the wire format, not the words a reader
// selects). Both ids the renderer mints (turn ids, the outcome anchor) and
// the text a reader can select exist ONLY in the interpreted
// `session-view/1` surface this module builds at serve time — so a
// `Section{id:"t-<uuid12>"}` or `Selection{snippet:"…rendered prose…"}`
// comment anchor on a `memory-session` capture could NEVER resolve
// Fresh/Exact against the raw resolver: every reindex would flip it to
// Stale (a false positive — the target is still there, just not in the
// bytes being searched). `resolve_capture_anchor` is the fix: a
// CATEGORY-GATED resolution rule the indexer calls instead of the generic
// resolver for `memory-session` docs. It changes ONLY what counts as
// Exact/Fuzzy/Stale (invariant #6 — `reanchor` stays the only anchor
// rewrite); it writes nothing.

/// Build the [`SessionView`] the indexer's stale-anchor pass resolves
/// against for one capture — decode the byte-identical `<pre>` transcript
/// and run it through this module's OWN engine, the same call
/// `session_render::render_if_session` makes at request time, so anchor
/// resolution sees exactly the ids/text a reader's browser would. Callers
/// (the indexer's open-comment loop) build this ONCE per document and reuse
/// it across every comment, mirroring the raw-HTML resolver's shared
/// `scraper::Html` slot (`review::fuzzy_resolve_anchor_with`). A capture
/// that fails to decode (hand-edited/truncated `<pre>`) yields an EMPTY view
/// (zero turns) rather than panicking — every anchor against it correctly
/// resolves Stale instead of crashing a reindex.
pub fn session_view_for_capture_html(html: &str) -> SessionView {
    let jsonl = super::recover_jsonl_from_capture(html).unwrap_or_default();
    let tail = TailBlocks::from_html(html);
    session_view(&jsonl, &tail, &ViewOptions::default())
}

/// The category-gated resolution rule itself. Two anchor kinds get the
/// session-aware treatment (memo R2's exact scope — Section AND Selection):
///
/// - `Section{id}`: Exact when `id` names a live [`Turn::id`] (`t-<uuid12>`,
///   deterministic from the turn's seed record uuid — no DOM walk needed,
///   `turn_id_from_uuid` is a pure hash of bytes already in hand) OR the
///   outcome footer's [`crate::sessions::SES_OUTCOME_ANCHOR`] id (`"ses-
///   outcome"`, always emitted by the renderer once any turns exist);
///   Stale otherwise — including every pre-this-milestone `#turn-N`
///   ordinal anchor, which is the accepted, documented break (session_render
///   module docs' "Deep-link grammar change").
/// - `Selection{snippet}`: matched against [`session_text_blocks`] (the
///   DECODED prose/text every turn actually carries), not the raw escaped
///   JSONL — see [`resolve_selection_in_view`].
///
/// `File`/`Chapter` anchors are OUT of this fix's scope (memo R2 names only
/// Section + Selection) and fall through unchanged to the ordinary
/// byte-level resolver — a session capture's raw HTML has exactly one
/// heading (`<h1>`) and no interesting DOM, so Chapter resolution there was
/// already effectively dead; File is `Exact("file")` everywhere regardless.
pub fn resolve_capture_anchor(
    view: &SessionView,
    html: &str,
    anchor: &review::Anchor,
) -> review::Resolution {
    match anchor {
        review::Anchor::Section { id, .. } => {
            if id == crate::sessions::SES_OUTCOME_ANCHOR || view.turns.iter().any(|t| &t.id == id) {
                review::Resolution::Exact(id.clone())
            } else {
                review::Resolution::Stale
            }
        }
        review::Anchor::Selection { snippet, .. } => resolve_selection_in_view(view, snippet),
        review::Anchor::File | review::Anchor::Chapter { .. } => {
            review::fuzzy_resolve_anchor(html, anchor)
        }
    }
}

/// Every decoded prose/text block a reader could actually select inside the
/// rendered transcript — the Selection-anchor matching corpus. Mirrors
/// `review::resolve_selection_in`'s block-level shape (one candidate per
/// paragraph-ish unit) but drawn from the interpreted [`Item`] tree instead
/// of `<p>/<li>/…>` DOM elements, since a capture's raw HTML has none of
/// those. Deliberately broad (every textual [`Item`] variant, plus the
/// header's opening/outcome quotes) — a Selection anchor can originate from
/// any rendered card, not only prose paragraphs.
fn session_text_blocks(view: &SessionView) -> Vec<String> {
    let mut blocks = Vec::new();
    if let Some(opening) = &view.header.opening {
        blocks.push(opening.text.clone());
    }
    if let Some(outcome) = &view.header.outcome {
        blocks.push(outcome.text.clone());
    }
    for turn in &view.turns {
        for item in &turn.items {
            match item {
                Item::Prose { text } => blocks.push(text.clone()),
                Item::Thinking { text: Some(t), .. } => blocks.push(t.clone()),
                Item::Decision { prompt, answer } => {
                    blocks.push(prompt.clone());
                    if let Some(a) = answer {
                        blocks.push(a.clone());
                    }
                }
                Item::Command {
                    stdout: Some(s), ..
                } => blocks.push(s.clone()),
                Item::WorkflowCard {
                    description: Some(d),
                    ..
                } => blocks.push(d.clone()),
                Item::SystemReminder { preview } => blocks.push(preview.clone()),
                Item::MemoryInjection { items } => blocks.extend(items.iter().cloned()),
                Item::ToolCall {
                    result: Some(r), ..
                } => blocks.push(r.preview.clone()),
                _ => {}
            }
        }
    }
    blocks
}

/// Selection-scope resolution over [`session_text_blocks`] — the same
/// exact-then-Jaro-Winkler ladder as `review::resolve_selection_in`
/// (reusing its `pub(crate)` scoring primitives — `context_chars`,
/// `jaro_winkler`, `fuzzy_threshold` — so a selection anchor scores
/// identically whether it lands on an ordinary artifact or a capture), minus
/// the structural-`css_path` tiebreak: session cards have no CSS path, and
/// near-duplicate prose within one transcript is rare enough that the plain
/// top-score winner is adequate for a resolution rule (never a rewrite).
fn resolve_selection_in_view(view: &SessionView, snippet: &str) -> review::Resolution {
    let ctx = review::context_chars();
    let needle: String = snippet.chars().take(ctx).collect();
    let needle_trim = needle.trim();
    if needle_trim.is_empty() {
        return review::Resolution::Stale;
    }
    let mut best_exact = false;
    let mut best_score: f32 = 0.0;
    let mut best_text = String::new();
    for block in session_text_blocks(view) {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == needle_trim {
            best_exact = true;
            best_text = trimmed.to_string();
            break;
        }
        let score = review::jaro_winkler(&needle, trimmed);
        if score > best_score {
            best_score = score;
            best_text = trimmed.to_string();
        }
    }
    if best_exact {
        review::Resolution::Exact(snippet.to_string())
    } else if best_score >= review::fuzzy_threshold() {
        review::Resolution::Fuzzy(best_text, best_score)
    } else {
        review::Resolution::Stale
    }
}

// ─── memory-recall ledger (MI-W1.1) ────────────────────────────────────────

/// One derived row for the `memory_recalls` ledger (V0035/V0037) — a single
/// recalled hit found inside a turn's `Item::MemoryInjection`. Pure
/// derivation over the already-interpreted [`SessionView`]: no storage
/// access, no clock. The enrichment hook stamps `session_id`/`artifact_id`
/// and writes the result through the storage actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedRecall {
    pub memory_kb: String,
    pub memory_id: String,
    pub turn_id: String,
    /// Unix seconds, when the enclosing turn carried a parseable timestamp —
    /// a close-but-not-exact proxy for the injection's own JSONL-line
    /// timestamp: `Item::MemoryInjection` carries no per-item ts (see
    /// `ingest_attachment`'s `pending_pre_items` — an injection is folded
    /// into whichever turn opens next).
    pub recalled_at: Option<i64>,
    /// CT-C5 (V0037) — `true` iff a turn STRICTLY AFTER this hit's own turn
    /// contains the memory's 12-hex id or its injected title verbatim (see
    /// [`was_explicitly_referenced`]). This measures EXPLICIT REFERENCE
    /// only: an agent can read and act on a recalled fact without ever
    /// naming it, and that case is indistinguishable from "unused" here —
    /// deliberately, since anything stronger would need an LLM judge
    /// (kb's no-in-daemon-LLM non-goal). Never scored: this field has no
    /// consumer on the `RecallHit`/rerank path, only the census read.
    pub used: bool,
    /// MR1 (SL6) — the hit's RANK in the recall pack that injected it
    /// (1 = top), read from the marker's `pos=` pair and nothing else.
    ///
    /// `None` is the honest answer in three cases, all of them real: a
    /// pre-MR1 capture (the marker had no `pos=`), a hit that only the
    /// free-text fallback could parse, and a mangled `pos` outside
    /// [`RECALL_MARKER_POS_RANGE`]. It is deliberately NOT inferred from
    /// the hit's index inside `Item::MemoryInjection`: under layout
    /// `v2-last` the pack is printed in reverse, so position in the
    /// transcript is not rank, and there is no way to tell the two layouts
    /// apart after the fact. The marker is the only witness.
    ///
    /// SURFACED-NEVER-SCORED, like `used`: it rides the ledger and the
    /// `recalled-by` read for display, and is structurally unreachable
    /// from `crate::memory`'s scoring types.
    pub pos: Option<u32>,
}

/// Parse `(title, memory_kb, memory_id)` out of one recalled hit's text,
/// e.g. `"- ingest retry cap (2026-05-02)  [notes]  (id 4c1d9a77bb21,
/// unread)"` — the exact shape `plugins/kb-memory/hooks/kb-recall.sh`'s jq
/// filter produces. Together with the CT-A3 machine marker this is the ONLY
/// place a memory's structured id (or its title) survives into the injected
/// transcript text (`Item::MemoryInjection` has no id/title field of its
/// own). `None` on anything that doesn't match (a hand-edited transcript,
/// or a future kb-recall.sh format change) — best-effort, never panics.
///
/// The search is anchored on the `"(id "` marker FIRST, then walks backward
/// for the bracket pair immediately preceding it — never a bare first-`[`
/// scan. `kb-recall.sh` interpolates the memory's free-text TITLE unescaped
/// before the bracketed kb tag, so a title containing its own `[`/`]` (e.g.
/// `"- Fixed [urgent] thing  [memory-kb]  (id …)"`) would otherwise pair the
/// title's own brackets and mis-parse the kb as the title fragment between
/// them instead of the real tag. The title is everything before that
/// bracket pair, minus the leading `"- "` bullet marker (and, since CT-C1,
/// a possible `"⚠ disputed: "` flag prefix).
fn parse_recall_item_parts(item: &str) -> Option<(String, String, String)> {
    let first_line = item.lines().next()?;
    let marker = "(id ";
    let marker_start = first_line.find(marker)?;
    let id_start = marker_start + marker.len();
    let id: String = first_line[id_start..].chars().take(12).collect();
    let is_lower_hex = id.len() == 12
        && id
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if !is_lower_hex {
        return None;
    }
    // The kb tag is the LAST complete bracket pair before the id marker.
    let prefix = &first_line[..marker_start];
    let kb_end = prefix.rfind(']')?;
    let kb_start = prefix[..kb_end].rfind('[')?;
    let kb = prefix[kb_start + 1..kb_end].trim();
    if kb.is_empty() {
        return None;
    }
    let title = prefix[..kb_start].trim();
    let title = title.strip_prefix("- ").unwrap_or(title).trim();
    let title = title.strip_prefix("⚠ disputed:").unwrap_or(title).trim();
    Some((title.to_string(), kb.to_string(), id))
}

/// Back-compat two-tuple shape for callers that only need `(memory_kb,
/// memory_id)` — see [`parse_recall_item_parts`] for the full parse
/// (including the title, used by [`was_explicitly_referenced`]). Test-only:
/// `derive_memory_recalls` (the one production caller) needs the title too,
/// so it calls `parse_recall_item_parts` directly.
#[cfg(test)]
fn parse_recall_item(item: &str) -> Option<(String, String)> {
    parse_recall_item_parts(item).map(|(_, kb, id)| (kb, id))
}

/// CT-C5 — a title shorter than this is too collision-prone to trust as an
/// explicit-reference signal on its own (e.g. a title like "fix" or "retry"
/// would match almost any later turn) — title-matching is skipped entirely
/// below this length (char count, not bytes), while id-matching (always a
/// full 12-hex token) is unaffected.
const MIN_TITLE_LEN_FOR_REFERENCE_MATCH: usize = 12;

/// The item kinds counted as "explicit reference" text for the CT-C5
/// used-recall scan: [`Item::Prose`] (the ONLY item that carries a human's
/// own typed prompt, and also an assistant's rendered reply text) and
/// [`Item::Thinking`]'s text (assistant reasoning) and [`Item::Decision`]'s
/// `prompt`/`answer` (an AskUserQuestion round-trip — genuine authored text
/// on both sides). Deliberately narrower than [`session_text_blocks`] (which
/// also indexes tool stdout/results, system-reminders, and workflow-card
/// descriptions for ANCHOR resolution): those are agent/tool OUTPUT, not the
/// agent or human naming the memory, and folding them in would credit "used"
/// to a memory whose id/title merely echoed through a grep hit or a pasted
/// file. `Item::MemoryInjection` text is likewise excluded — a re-recall of
/// the SAME memory in a later turn is its own, independent `DerivedRecall`,
/// not a "reference" to this one.
fn explicit_reference_texts(turn: &Turn) -> impl Iterator<Item = &str> {
    turn.items.iter().flat_map(|item| {
        let texts: Vec<&str> = match item {
            Item::Prose { text } => vec![text.as_str()],
            Item::Thinking { text: Some(t), .. } => vec![t.as_str()],
            Item::Decision { prompt, answer } => {
                let mut v = vec![prompt.as_str()];
                if let Some(a) = answer {
                    v.push(a.as_str());
                }
                v
            }
            _ => Vec::new(),
        };
        texts
    })
}

/// CT-C5 — did any turn in `later_turns` (already sliced to STRICTLY AFTER
/// the injection's own turn — a reference in the SAME or an EARLIER turn
/// does not count, since that's what triggered the recall, not a response to
/// it) explicitly name this memory? Two independent, case-sensitive,
/// verbatim-substring signals, either sufficient: the 12-hex `memory_id`, or
/// `title` when it clears [`MIN_TITLE_LEN_FOR_REFERENCE_MATCH`]. No fuzzy
/// matching — an agent paraphrasing the title in different words or case is
/// treated as not referencing it (a conservative false-negative bias is the
/// honest default for an "explicit reference" signal; see
/// [`DerivedRecall::used`]). A marker-parsed hit whose human line didn't
/// yield a title passes `""` here — title-matching simply never fires and
/// id-matching carries the signal alone.
fn was_explicitly_referenced(later_turns: &[Turn], memory_id: &str, title: &str) -> bool {
    let title_matchable = title.chars().count() >= MIN_TITLE_LEN_FOR_REFERENCE_MATCH;
    for turn in later_turns {
        for text in explicit_reference_texts(turn) {
            if text.contains(memory_id) {
                return true;
            }
            if title_matchable && text.contains(title) {
                return true;
            }
        }
    }
    false
}

/// Parse `(memory_kb, memory_id, pos)` out of a CT-A3 `kb-recall/1` machine
/// marker line — `<!--kb-recall/1 kb=<kb-name> id=<hex12>[ pos=<n>]-->` —
/// appended by `plugins/kb-memory/hooks/kb-recall.sh` right after each
/// recalled hit's human-readable line and folded into that hit's `items[]`
/// entry by `ingest_attachment` (same fold as the `↳` summary
/// continuation). This is the PREFERRED source: unlike
/// [`parse_recall_item_parts`]'s free-text-anchored grammar, it survives a
/// future reformat of the human-readable line — and MR1's layout v2, which
/// deletes the `(id …)` parenthetical the free-text grammar is anchored on,
/// is exactly that reformat happening.
///
/// MR1 grammar (widened from the pre-MR1 single `split_once(" id=")`, which
/// would have REJECTED the trailing `pos=` pair and silently zeroed the
/// ledger the moment layout v2 shipped): the marker body is split on ASCII
/// whitespace into `key=value` pairs, order-independent.
///
/// * `kb` — required, non-empty.
/// * `id` — required, exactly 12 lowercase hex chars. A malformed id is the
///   ONE thing that fails the whole marker: it is the row's identity, and a
///   row keyed on a half-read id is worse than a counted `failed`.
/// * `pos` — optional, `1..=99` ([`RECALL_MARKER_POS_RANGE`]). Out of
///   range, unparseable, or absent all read as `None` — an unknown rank,
///   never a guessed one, and never a reason to drop the row.
/// * anything else — IGNORED, so the grammar is forward-compatible: a
///   future hook may add pairs without a kb-core release having to land
///   first. A bare token with no `=` is ignored on the same principle.
///
/// Scans every physical line of `item` (not just the first, since the
/// marker rides its own line after the bullet ± summary). `None` on
/// anything that doesn't match — an older kb-recall.sh (pre-CT-A3), a
/// hand-edited transcript, or a mangled marker — never panics;
/// [`derive_memory_recalls`] falls back to [`parse_recall_item_parts`] in
/// that case.
fn parse_recall_marker(item: &str) -> Option<(String, String, Option<u32>)> {
    item.lines().find_map(|line| {
        let line = line.trim();
        let body = line
            .strip_prefix(RECALL_MARKER_PREFIX)?
            .strip_suffix(RECALL_MARKER_SUFFIX)?;
        let mut kb: Option<&str> = None;
        let mut id: Option<&str> = None;
        let mut pos: Option<u32> = None;
        for pair in body.split_ascii_whitespace() {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            match key {
                "kb" => kb = Some(value),
                "id" => id = Some(value),
                "pos" => {
                    pos = value
                        .parse::<u32>()
                        .ok()
                        .filter(|n| RECALL_MARKER_POS_RANGE.contains(n))
                }
                _ => {}
            }
        }
        let kb = kb?.trim();
        let id = id?;
        let is_lower_hex = id.len() == 12
            && id
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        if kb.is_empty() || !is_lower_hex {
            return None;
        }
        Some((kb.to_string(), id.to_string(), pos))
    })
}

/// Outcome of deriving the `memory_recalls` ledger rows from one
/// [`SessionView`] (see [`derive_memory_recalls`]) — the row set PLUS a
/// three-way parse census (CT-A3) so a caller (the enrichment hook, and
/// eventually `kb doctor`) can tell "no recalls happened this capture"
/// apart from "recalls happened but the parse silently dropped them".
/// `marker_parsed + fallback_parsed + failed` is the total number of
/// injected hits walked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedRecalls {
    pub rows: Vec<DerivedRecall>,
    /// Hits whose [`parse_recall_marker`] succeeded (the preferred path).
    pub marker_parsed: usize,
    /// Hits with no marker (or a mangled one) that still parsed via
    /// [`parse_recall_item_parts`]'s free-text grammar.
    pub fallback_parsed: usize,
    /// Hits that parsed via NEITHER grammar — a ledger gap: the injection
    /// happened but yielded no row.
    pub failed: usize,
}

/// Walk every turn's `Item::MemoryInjection` items and derive one
/// [`DerivedRecall`] per hit whose text parses — preferring the CT-A3
/// [`parse_recall_marker`] machine marker, falling back permanently to
/// [`parse_recall_item_parts`]'s free-text grammar when no marker is
/// present (an older capture, or a hand-edited transcript). A hit that
/// parses via NEITHER is silently dropped from `rows` (the ledger is a
/// best-effort census, not a strict transcript audit) but still counted in
/// [`DerivedRecalls::failed`], so a caller can tell the gap apart from "no
/// recalls this capture". The `↳ <summary>` fold in `ingest_attachment`
/// guarantees one `items[]` entry per recalled hit (not per physical line),
/// so this never double-counts a summary-bearing hit.
///
/// `used` (CT-C5) is derived in the SAME pass, scanning only turns strictly
/// after this hit's own — see [`was_explicitly_referenced`]. The title fed
/// to the title-match half always comes from the human line's
/// [`parse_recall_item_parts`] (the marker carries no title); a
/// marker-parsed hit whose human line doesn't parse degrades to
/// id-matching alone.
pub fn derive_memory_recalls(view: &SessionView) -> DerivedRecalls {
    let mut out = DerivedRecalls::default();
    for (turn_idx, turn) in view.turns.iter().enumerate() {
        let recalled_at = turn.ts.as_deref().and_then(crate::timeparse::parse_iso_utc);
        for item in &turn.items {
            let Item::MemoryInjection { items } = item else {
                continue;
            };
            for hit in items {
                let parts = parse_recall_item_parts(hit);
                // `pos` rides the MARKER only — the free-text fallback has
                // no rank to give, and the hit's index here is not one
                // either (layout v2-last prints the pack reversed).
                let (parsed, via_marker) = match parse_recall_marker(hit) {
                    Some(kb_id_pos) => (Some(kb_id_pos), true),
                    None => (
                        parts
                            .as_ref()
                            .map(|(_, kb, id)| (kb.clone(), id.clone(), None)),
                        false,
                    ),
                };
                match parsed {
                    Some((memory_kb, memory_id, pos)) => {
                        if via_marker {
                            out.marker_parsed += 1;
                        } else {
                            out.fallback_parsed += 1;
                        }
                        let title = parts.as_ref().map(|(t, _, _)| t.as_str()).unwrap_or("");
                        let later_turns = &view.turns[turn_idx + 1..];
                        let used = was_explicitly_referenced(later_turns, &memory_id, title);
                        out.rows.push(DerivedRecall {
                            memory_kb,
                            memory_id,
                            turn_id: turn.id.clone(),
                            recalled_at,
                            used,
                            pos,
                        });
                    }
                    None => out.failed += 1,
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/session_fixtures")
            .join(format!("{name}.jsonl"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
    }

    fn view(name: &str) -> SessionView {
        let jsonl = fixture(name);
        session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default())
    }

    // --- grammar + tolerance floor -------------------------------------

    #[test]
    fn grammar_is_pinned() {
        assert_eq!(VIEW_GRAMMAR, "session-view/1");
        assert_eq!(view("synthetic-workflow-heavy").grammar, VIEW_GRAMMAR);
    }

    #[test]
    fn garbage_input_still_renders_something() {
        let v = session_view(
            "not json at all\n{{{\nnull\n",
            &TailBlocks::default(),
            &ViewOptions::default(),
        );
        assert_eq!(v.grammar, VIEW_GRAMMAR);
        assert!(!v.turns.is_empty(), "garbage still yields a Raw turn");
        assert!(v
            .turns
            .iter()
            .any(|t| t.items.iter().any(|i| matches!(i, Item::Raw { .. }))));
    }

    #[test]
    fn empty_input_yields_empty_but_valid_view() {
        let v = session_view("", &TailBlocks::default(), &ViewOptions::default());
        assert!(v.turns.is_empty());
        assert!(v.outline.is_empty());
        assert!(v.header.outcome.is_none());
    }

    // --- the join pass ----------------------------------------------------

    #[test]
    fn request_id_merge_collapses_the_shatter_and_keeps_ghost_thinking_as_a_glyph() {
        let v = view("synthetic-workflow-heavy");
        // req_A9 spans a real Prose closing line AND a trailing empty
        // Thinking fragment — ONE turn, not two.
        let closing_turn = v
            .turns
            .iter()
            .find(|t| {
                t.items
                    .iter()
                    .any(|i| matches!(i, Item::Prose { text } if text.starts_with("Done.")))
            })
            .expect("closing turn present");
        assert!(closing_turn
            .items
            .iter()
            .any(|i| matches!(i, Item::Thinking { empty: true, .. })));
        assert_eq!(
            closing_turn
                .items
                .iter()
                .filter(|i| matches!(i, Item::Prose { .. }))
                .count(),
            1,
            "the requestId-shatter must not duplicate the Prose item"
        );
    }

    #[test]
    fn tool_call_pairing_folds_result_into_the_call_and_user_role_never_becomes_a_turn() {
        let v = view("synthetic-kb-commands");
        // Every user-role Turn must have real Prose content — pure
        // tool_result carriers never become a Turn (Role::Human reserved
        // for humans).
        for t in v.turns.iter().filter(|t| t.role == Role::Human) {
            assert!(
                !t.items.is_empty(),
                "a Human turn must carry a real item, never be a bare result carrier"
            );
        }
        // The Read call for retry.rs got its numbered-line result folded in.
        let read = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .find(|i| matches!(i, Item::ToolCall { name, .. } if name == "Read"))
            .expect("Read call present");
        let Item::ToolCall {
            result, unpaired, ..
        } = read
        else {
            unreachable!()
        };
        assert!(!unpaired);
        assert!(result.as_ref().unwrap().preview.contains("MAX_ATTEMPTS"));
    }

    #[test]
    fn unpaired_tool_call_is_flagged_never_dropped() {
        let jsonl = [
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"echo hi"}}]}}"#,
        ].join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let call = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .find(|i| matches!(i, Item::ToolCall { .. }))
            .expect("call present even with no result");
        let Item::ToolCall {
            unpaired, result, ..
        } = call
        else {
            unreachable!()
        };
        assert!(unpaired);
        assert!(result.is_none());
    }

    #[test]
    fn task_lifecycle_threads_create_update_and_notification_to_one_board_row() {
        let v = view("synthetic-workflow-heavy");
        // Task "1" (Recon TaskCreate → TaskUpdate x2) ends completed.
        let recon = v
            .tasks_final
            .iter()
            .find(|r| r.id == "1")
            .expect("task 1 present on the final board");
        assert_eq!(recon.status, "completed");
        // The workflow's own background task (wf7k2m9x) is a SEPARATE id,
        // threaded from the Workflow launch result + two task-notifications.
        assert!(v.tasks_final.iter().any(|r| r.id == "wf7k2m9x"));
        // At least one TaskEvent item exists in-flow (task-notification join).
        assert!(v.turns.iter().flat_map(|t| &t.items).any(|i| matches!(
            i,
            Item::TaskEvent { id, .. } if id == "wf7k2m9x"
        )));
    }

    #[test]
    fn sidechain_run_becomes_one_side_lane() {
        let v = view("synthetic-workflow-heavy");
        assert_eq!(
            v.side_lanes.len(),
            1,
            "exactly one sidechain run in the fixture"
        );
        assert!(!v.side_lanes[0].turn_ids.is_empty());
    }

    // --- interpretation catalog (R14) --------------------------------------

    #[test]
    fn caveat_is_suppressed_command_is_a_chip_and_task_notification_is_not_a_prompt() {
        let v = view("synthetic-workflow-heavy");
        // The caveat never appears as Prose anywhere.
        assert!(!v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .any(|i| matches!(i, Item::Prose { text } if text.contains("Caveat:"))));
        // The /model command became a Command chip.
        assert!(v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .any(|i| matches!(i, Item::Command { name, .. } if name.contains("model"))));
        // Outline (real prompts only) contains the real typed ask, not the
        // caveat/command wrappers or the task-notification lines.
        assert_eq!(v.outline.len(), 1);
        assert!(v.outline[0]
            .preview
            .starts_with("Build the atlas fit workflow"));
    }

    #[test]
    fn header_opening_uses_the_wrapper_skipped_typed_prompt_r14() {
        let v = view("synthetic-workflow-heavy");
        let opening = v.header.opening.as_ref().expect("opening present");
        assert!(opening.text.starts_with("Build the atlas fit workflow"));
        assert!(!opening.text.contains("Caveat"));
    }

    #[test]
    fn recall_memory_injection_becomes_a_chip_not_a_hook_dump() {
        let v = view("synthetic-kb-commands");
        let injections: Vec<&Item> = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .filter(|i| matches!(i, Item::MemoryInjection { .. }))
            .collect();
        assert_eq!(injections.len(), 1);
        let Item::MemoryInjection { items } = injections[0] else {
            unreachable!()
        };
        assert_eq!(items.len(), 2);
        assert!(items[0].contains("ingest retry cap"));
        // MI-W1.1 regression: the fixture now carries a `↳ <summary>`
        // continuation line after EACH hit — one item per hit, not one per
        // line, is the whole point of the fold.
        assert!(items[0].contains('\n'), "summary folded into hit 0");
        assert!(items[0].contains("May incident review"));
        assert!(items[1].contains("worker's own retry loop"));
    }

    /// MI-W1.1 — a `↳ <summary>` continuation line right after a `- <hit>`
    /// line must fold INTO that hit's `items[]` entry (joined by `\n`), not
    /// become a separate `MemoryInjection.items` entry of its own. A flat
    /// per-line walk would turn N recalled hits with summaries into 2N
    /// items, double-counting every hit for any consumer (e.g. the
    /// injection-ledger census) that treats `items.len()` as "hits
    /// injected".
    #[test]
    fn recall_memory_injection_folds_summary_continuation_lines_into_the_hit() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- alpha fact  [notes]  (id aaaaaaaaaaaa)\n    ↳ alpha summary text\n- beta fact  [notes]  (id bbbbbbbbbbbb, unread)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"20000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"fold-test-1"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"20000002-0000-4000-8000-000000000002","parentUuid":"20000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z","sessionId":"fold-test-1"}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let injections: Vec<&Item> = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .filter(|i| matches!(i, Item::MemoryInjection { .. }))
            .collect();
        assert_eq!(
            injections.len(),
            1,
            "one MemoryInjection item, not one per line"
        );
        let Item::MemoryInjection { items } = injections[0] else {
            unreachable!()
        };
        // Two hits went in — exactly two items must come out, even though
        // the first hit spans two physical lines (hit + `↳` summary).
        assert_eq!(
            items.len(),
            2,
            "one items[] entry PER HIT, summary folded in"
        );
        assert!(items[0].contains("alpha fact"));
        assert!(items[0].contains("alpha summary text"));
        assert!(items[0].contains('\n'), "hit + summary joined by newline");
        assert!(items[1].contains("beta fact"));
        assert!(
            !items[1].contains('\n'),
            "the summary-less second hit stays single-line"
        );
    }

    #[test]
    fn parse_recall_item_extracts_kb_and_lowercase_hex_id() {
        assert_eq!(
            parse_recall_item("- ingest retry cap (2026-05-02)  [notes]  (id 4c1d9a77bb21)"),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string()))
        );
        // A trailing read-state suffix (", read 40%" / ", unread") doesn't
        // interfere — only the first 12 chars after "(id " are taken.
        assert_eq!(
            parse_recall_item("- ingest backoff (2026-05-19)  [notes]  (id 91fe02aa10cd, unread)"),
            Some(("notes".to_string(), "91fe02aa10cd".to_string()))
        );
        // The summary fold means a real item can carry a second line —
        // only the FIRST line is parsed.
        assert_eq!(
            parse_recall_item("- alpha  [notes]  (id aaaaaaaaaaaa)\n    ↳ a summary"),
            Some(("notes".to_string(), "aaaaaaaaaaaa".to_string()))
        );
    }

    #[test]
    fn parse_recall_item_rejects_malformed_or_non_hex_shapes() {
        assert_eq!(parse_recall_item("just some prose, no brackets"), None);
        assert_eq!(
            parse_recall_item("- title  [notes]  no id marker here"),
            None
        );
        // Uppercase hex / too-short id / missing brackets all fail.
        assert_eq!(
            parse_recall_item("- title  [notes]  (id AAAAAAAAAAAA)"),
            None
        );
        assert_eq!(parse_recall_item("- title  [notes]  (id abc)"), None);
        assert_eq!(parse_recall_item("- title  (id aaaaaaaaaaaa)"), None);
        assert_eq!(parse_recall_item("- title  []  (id aaaaaaaaaaaa)"), None);
    }

    /// A memory TITLE is free text and `kb-recall.sh` interpolates it
    /// unescaped before the bracketed kb tag — a title containing its own
    /// `[`/`]` must not be mistaken for the kb tag. The parse anchors on
    /// `"(id "` and walks backward for the bracket pair immediately
    /// preceding it, so the REAL `[memory-kb]` tag wins regardless of what
    /// brackets appear earlier in the title.
    #[test]
    fn parse_recall_item_is_robust_to_brackets_in_the_title() {
        assert_eq!(
            parse_recall_item("- Fixed [urgent] thing  [memory-kb]  (id 0123456789ab, unread)"),
            Some(("memory-kb".to_string(), "0123456789ab".to_string()))
        );
        // Multiple bracketed fragments in the title — still the LAST pair
        // before "(id " wins.
        assert_eq!(
            parse_recall_item("- [a] then [b] title  [notes]  (id aaaaaaaaaaaa)"),
            Some(("notes".to_string(), "aaaaaaaaaaaa".to_string()))
        );
    }

    /// CT-A3 — `parse_recall_marker` golden: the exact shape
    /// `kb-recall.sh` appends, on its own line or trailing a bullet
    /// ± summary (the `\n`-joined `items[]` entry `ingest_attachment`
    /// folds it into).
    #[test]
    fn parse_recall_marker_extracts_kb_and_lowercase_hex_id() {
        // The PRE-MR1 two-pair form (layout v1, and every capture on disk
        // before MR1 shipped). It must keep parsing forever, and its `pos`
        // is honestly absent rather than inferred.
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes id=4c1d9a77bb21-->"),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string(), None))
        );
        // Folded onto a hit's bullet ± summary text (the shape it actually
        // arrives in via `ingest_attachment`'s fold).
        assert_eq!(
            parse_recall_marker(
                "- ingest retry cap  [notes]  (id 4c1d9a77bb21)\n    ↳ a summary\n<!--kb-recall/1 kb=notes id=4c1d9a77bb21-->"
            ),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string(), None))
        );
        // A kb name containing a hyphen (a valid `KbName`) round-trips.
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=memory-kb id=0123456789ab-->"),
            Some(("memory-kb".to_string(), "0123456789ab".to_string(), None))
        );
    }

    /// MR1 golden — the THREE-pair form layout v2 emits. `pos` is the
    /// hit's rank in the pack (1 = top), and it is the ONLY witness to
    /// that rank: under layout `v2-last` the same pack prints in reverse,
    /// so `pos=1` legitimately arrives on the LAST bullet of the block.
    #[test]
    fn parse_recall_marker_reads_the_pos_pair() {
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes id=4c1d9a77bb21 pos=1-->"),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string(), Some(1)))
        );
        // Layout v2's own five-hit shape, folded exactly as
        // `ingest_attachment` hands it over (bullet, deep summary, marker).
        assert_eq!(
            parse_recall_marker(
                "- demo-repo build cache setup  [kb]\n    ↳ installed 2026-08-01\n<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=3-->"
            ),
            Some(("kb".to_string(), "a1b2c3d4e5f6".to_string(), Some(3)))
        );
        // The top of the range, and a title-only hit (no `↳` line at all —
        // ranks 4-5 under layout v2).
        assert_eq!(
            parse_recall_marker(
                "- fifth title  [kb]\n<!--kb-recall/1 kb=kb id=0123456789ab pos=99-->"
            ),
            Some(("kb".to_string(), "0123456789ab".to_string(), Some(99)))
        );
    }

    /// MR1 — the body is an UNORDERED bag of `key=value` pairs and unknown
    /// keys are ignored, so a future hook can add pairs without a kb-core
    /// release having to land first. The pre-MR1 grammar
    /// (`split_once(" id=")`) would have rejected every one of these — and
    /// would have rejected layout v2's own `pos=` marker, silently zeroing
    /// the ledger the day v2 shipped. That is the regression this test
    /// exists to prevent.
    #[test]
    fn parse_recall_marker_is_order_independent_and_ignores_unknown_pairs() {
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 id=4c1d9a77bb21 kb=notes-->"),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string(), None)),
            "key order is not part of the grammar"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 pos=2 id=4c1d9a77bb21 kb=notes-->"),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string(), Some(2)))
        );
        assert_eq!(
            parse_recall_marker(
                "<!--kb-recall/1 kb=notes id=4c1d9a77bb21 pos=2 salience=0.9 tier=hot-->"
            ),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string(), Some(2))),
            "unknown pairs are ignored, not fatal"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes bareword id=4c1d9a77bb21-->"),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string(), None)),
            "a token with no '=' is ignored on the same principle"
        );
        // Extra internal whitespace is not significant.
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes   id=4c1d9a77bb21  pos=5-->"),
            Some(("notes".to_string(), "4c1d9a77bb21".to_string(), Some(5)))
        );
    }

    /// MR1 — a mangled `pos` degrades to "rank unknown", it never costs the
    /// row. `kb` and `id` are what the ledger row is FOR; a display-only
    /// rank that failed to parse is not worth losing the recall over.
    #[test]
    fn parse_recall_marker_drops_an_out_of_range_or_unparseable_pos_but_keeps_the_row() {
        for body in [
            "pos=0",   // below the range — ranks are 1-based
            "pos=100", // above RECALL_MARKER_POS_RANGE
            "pos=-1",
            "pos=three",
            "pos=",
            "pos=1.5",
        ] {
            assert_eq!(
                parse_recall_marker(&format!(
                    "<!--kb-recall/1 kb=notes id=4c1d9a77bb21 {body}-->"
                )),
                Some(("notes".to_string(), "4c1d9a77bb21".to_string(), None)),
                "{body} must degrade to pos: None, not fail the marker"
            );
        }
    }

    /// CT-A3 — malformed / mangled markers all reject rather than
    /// mis-parsing (never panics): missing suffix, uppercase hex, a
    /// too-short id, an empty kb, or plain prose with no marker at all.
    #[test]
    fn parse_recall_marker_rejects_malformed_or_non_hex_shapes() {
        assert_eq!(
            parse_recall_marker("just some prose, no marker at all"),
            None
        );
        assert_eq!(
            parse_recall_marker("- ingest retry cap  [notes]  (id 4c1d9a77bb21)"),
            None,
            "no marker line present at all — a pre-CT-A3 capture"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes id=4c1d9a77bb21"),
            None,
            "missing the closing '-->'"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes id=4C1D9A77BB21-->"),
            None,
            "uppercase hex rejected"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes id=abc-->"),
            None,
            "too-short id rejected"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb= id=4c1d9a77bb21-->"),
            None,
            "empty kb rejected"
        );
        // MR1 — a malformed id is still the ONE fatal defect, and a valid
        // `pos` never rescues it: the row's identity is the id.
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes id=4C1D9A77BB21 pos=1-->"),
            None,
            "uppercase hex rejected even with a well-formed pos"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes id=abc pos=1-->"),
            None,
            "too-short id rejected even with a well-formed pos"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes id=4c1d9a77bb2g pos=1-->"),
            None,
            "a non-hex char anywhere in the id rejects the marker"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 kb=notes pos=1-->"),
            None,
            "an id-less marker is not a row"
        );
        assert_eq!(
            parse_recall_marker("<!--kb-recall/1 id=4c1d9a77bb21 pos=1-->"),
            None,
            "a kb-less marker is not a row"
        );
    }

    /// MR1 — a layout-v2 five-hit block: every hit parses via the marker,
    /// and `pos` arrives 1..5 in reading order. Ranks 4-5 carry NO `↳`
    /// summary line at all (the v2 cut), which is exactly where a fold bug
    /// would show up as a merged or missing item.
    #[test]
    fn derive_memory_recalls_reads_pos_from_a_layout_v2_block() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall — these persist across sessions):\n- demo-repo build cache setup  [kb]\n    ↳ a deep summary\n<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=1-->\n- Wrong port  [main] [⚠ 1 drift-flagged citation(s)]\n    ↳ another summary\n<!--kb-recall/1 kb=main id=0f0f0f0f0f0f pos=2-->\n- third title  [kb]\n    ↳ a shallower summary\n<!--kb-recall/1 kb=kb id=111111111111 pos=3-->\n- fourth title  [kb]\n<!--kb-recall/1 kb=kb id=222222222222 pos=4-->\n- fifth title  [main]\n<!--kb-recall/1 kb=main id=333333333333 pos=5-->"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"31000001-0000-4000-8000-000000000001","timestamp":"2026-09-05T09:00:05.000Z","sessionId":"mr1-v2"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"31000001-0000-4000-8000-000000000002","parentUuid":"31000001-0000-4000-8000-000000000001","timestamp":"2026-09-05T09:00:25.000Z","sessionId":"mr1-v2"}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 5, "one row per hit, marker-folded");
        assert_eq!(derived.marker_parsed, 5);
        assert_eq!(derived.fallback_parsed, 0);
        assert_eq!(derived.failed, 0);
        assert_eq!(
            derived.rows.iter().map(|r| r.pos).collect::<Vec<_>>(),
            vec![Some(1), Some(2), Some(3), Some(4), Some(5)]
        );
        assert_eq!(derived.rows[0].memory_id, "a1b2c3d4e5f6");
        assert_eq!(derived.rows[4].memory_id, "333333333333");
    }

    /// MR1 — the SAME pack under layout `v2-last`: the block is printed in
    /// reverse, so the transcript's item ORDER is 5,4,3,2,1 while every
    /// `pos` still names the true rank. This is the test that would fail if
    /// anyone ever "simplified" `pos` into the hit's index — the two
    /// disagree here by construction, and the marker is right.
    #[test]
    fn derive_memory_recalls_pos_is_the_rank_not_the_position_under_v2_last() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall — these persist across sessions):\n- fifth title  [main]\n<!--kb-recall/1 kb=main id=333333333333 pos=5-->\n- fourth title  [kb]\n<!--kb-recall/1 kb=kb id=222222222222 pos=4-->\n- third title  [kb]\n    ↳ a shallower summary\n<!--kb-recall/1 kb=kb id=111111111111 pos=3-->\n- Wrong port  [main] [⚠ 1 drift-flagged citation(s)]\n    ↳ another summary\n<!--kb-recall/1 kb=main id=0f0f0f0f0f0f pos=2-->\n- demo-repo build cache setup  [kb]\n    ↳ a deep summary\n<!--kb-recall/1 kb=kb id=a1b2c3d4e5f6 pos=1-->"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"32000001-0000-4000-8000-000000000001","timestamp":"2026-09-05T09:00:05.000Z","sessionId":"mr1-v2last"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"32000001-0000-4000-8000-000000000002","parentUuid":"32000001-0000-4000-8000-000000000001","timestamp":"2026-09-05T09:00:25.000Z","sessionId":"mr1-v2last"}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 5);
        assert_eq!(derived.marker_parsed, 5);
        assert_eq!(
            derived.rows.iter().map(|r| r.pos).collect::<Vec<_>>(),
            vec![Some(5), Some(4), Some(3), Some(2), Some(1)],
            "reading order is reversed; pos still carries the rank"
        );
        assert_eq!(
            derived.rows[0].memory_id, "333333333333",
            "the rank-5 hit is printed FIRST under v2-last"
        );
        assert_eq!(
            derived.rows[4].memory_id, "a1b2c3d4e5f6",
            "the rank-1 hit is printed LAST under v2-last"
        );
    }

    /// MR1 — a hit only the FREE-TEXT fallback can parse (a pre-CT-A3
    /// capture) yields `pos: None`. The fallback grammar has no rank to
    /// give and nothing infers one for it.
    #[test]
    fn derive_memory_recalls_leaves_pos_none_on_a_fallback_parse() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- ingest retry cap  [notes]  (id 4c1d9a77bb21, unread)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"33000001-0000-4000-8000-000000000001","timestamp":"2026-09-05T09:00:05.000Z","sessionId":"mr1-fallback"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"33000002-0000-4000-8000-000000000002","parentUuid":"33000001-0000-4000-8000-000000000001","timestamp":"2026-09-05T09:00:25.000Z","sessionId":"mr1-fallback"}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert_eq!(derived.fallback_parsed, 1);
        assert_eq!(derived.marker_parsed, 0);
        assert_eq!(derived.rows[0].pos, None);
    }

    /// MI-W1.1 — the pure derivation the `memory-recall-ledger` enrichment
    /// hook builds its rows from: walking `synthetic-kb-commands`'s two
    /// recalled hits (with their `↳` summaries, no CT-A3 marker — an
    /// old-format fixture) must yield exactly two `DerivedRecall`s via the
    /// FALLBACK grammar, ids/kbs parsed correctly, `recalled_at` set from
    /// the enclosing turn's timestamp.
    #[test]
    fn derive_memory_recalls_walks_every_injected_hit() {
        let v = view("synthetic-kb-commands");
        let derived = derive_memory_recalls(&v);
        assert_eq!(
            derived.rows.len(),
            2,
            "one DerivedRecall per hit, not per line"
        );
        assert_eq!(
            derived.marker_parsed, 0,
            "no marker in this old-format fixture"
        );
        assert_eq!(derived.fallback_parsed, 2);
        assert_eq!(derived.failed, 0);
        assert_eq!(derived.rows[0].memory_kb, "notes");
        assert_eq!(derived.rows[0].memory_id, "4c1d9a77bb21");
        assert_eq!(derived.rows[1].memory_kb, "notes");
        assert_eq!(derived.rows[1].memory_id, "91fe02aa10cd");
        // Both hits landed on the SAME turn (the injection precedes the
        // first human message) — recalled_at is the turn's own ts.
        assert_eq!(derived.rows[0].turn_id, derived.rows[1].turn_id);
        let expected_ts = crate::timeparse::parse_iso_utc("2026-06-04T09:00:25.000Z");
        assert_eq!(derived.rows[0].recalled_at, expected_ts);
        assert!(expected_ts.is_some());
    }

    /// A turn's `Item::MemoryInjection` items that fail to parse (unexpected
    /// shape, no marker either) are silently dropped from `rows` — never
    /// panicking and never producing a partial/garbage row — but still
    /// counted in `failed`, so a caller (the enrichment hook's warn) can
    /// tell "recalls happened but none parsed" apart from "no recalls".
    #[test]
    fn derive_memory_recalls_drops_unparseable_hits() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- a hand-edited line with no brackets at all"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"21000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"fold-test-2"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"21000002-0000-4000-8000-000000000002","parentUuid":"21000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z","sessionId":"fold-test-2"}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert!(derived.rows.is_empty());
        assert_eq!(derived.marker_parsed, 0);
        assert_eq!(derived.fallback_parsed, 0);
        assert_eq!(
            derived.failed, 1,
            "the one mangled hit is counted, not silently absorbed"
        );
    }

    /// CT-A3 — a NEW-format injection (marker present) parses via
    /// [`parse_recall_marker`] and is counted in `marker_parsed`, not
    /// `fallback_parsed`.
    #[test]
    fn derive_memory_recalls_prefers_the_marker_when_present() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- ingest retry cap  [notes]  (id 4c1d9a77bb21)\n    ↳ a summary\n<!--kb-recall/1 kb=notes id=4c1d9a77bb21-->"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"22000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"marker-test-1"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"22000002-0000-4000-8000-000000000002","parentUuid":"22000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z","sessionId":"marker-test-1"}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert_eq!(derived.marker_parsed, 1);
        assert_eq!(derived.fallback_parsed, 0);
        assert_eq!(derived.failed, 0);
        assert_eq!(derived.rows[0].memory_kb, "notes");
        assert_eq!(derived.rows[0].memory_id, "4c1d9a77bb21");
    }

    /// CT-A3 — when the human-readable line and the marker DISAGREE (a
    /// reformatted/garbled bullet the fallback grammar would parse
    /// differently, or not at all), the marker WINS. This is the whole
    /// point of the hardening: a future reformat of the free-text line
    /// can no longer silently corrupt or zero out the ledger.
    #[test]
    fn derive_memory_recalls_marker_wins_over_a_disagreeing_fallback_parse() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- ingest retry cap  [wrongkb]  (id ffffffffffff)\n<!--kb-recall/1 kb=notes id=4c1d9a77bb21-->"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"23000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"marker-test-2"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"23000002-0000-4000-8000-000000000002","parentUuid":"23000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z","sessionId":"marker-test-2"}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert_eq!(derived.marker_parsed, 1);
        assert_eq!(derived.fallback_parsed, 0);
        assert_eq!(
            derived.rows[0].memory_kb, "notes",
            "the marker's kb, not the fallback-parseable 'wrongkb'"
        );
        assert_eq!(derived.rows[0].memory_id, "4c1d9a77bb21");
    }

    /// CT-A3 — a mixed injection (one hit carries a marker, the sibling
    /// hit predates it / lost it) splits `marker_parsed`/`fallback_parsed`
    /// correctly per hit rather than an all-or-nothing choice per turn.
    #[test]
    fn derive_memory_recalls_counts_marker_and_fallback_hits_independently() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- alpha  [notes]  (id aaaaaaaaaaaa)\n<!--kb-recall/1 kb=notes id=aaaaaaaaaaaa-->\n- beta  [notes]  (id bbbbbbbbbbbb)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"24000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"marker-test-3"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"24000002-0000-4000-8000-000000000002","parentUuid":"24000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:25.000Z","sessionId":"marker-test-3"}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 2);
        assert_eq!(derived.marker_parsed, 1, "the alpha hit carried a marker");
        assert_eq!(
            derived.fallback_parsed, 1,
            "the beta hit fell back to the text grammar"
        );
        assert_eq!(derived.failed, 0);
    }

    // --- CT-C5: injection efficacy (used) ----------------------------------

    /// A later turn naming the memory's 12-hex id verbatim marks the hit
    /// `used` — the title here ("alpha", 5 chars) is deliberately under the
    /// title-match floor so this test isolates id-matching alone.
    #[test]
    fn derive_memory_recalls_marks_used_when_id_referenced_later() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- alpha  [notes]  (id aaaaaaaaaaaa)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"22000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"used-test-1"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"22000002-0000-4000-8000-000000000002","parentUuid":"22000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:06.000Z","sessionId":"used-test-1"}"#,
            r#"{"type":"assistant","timestamp":"2026-06-04T09:00:07.000Z","uuid":"22000003-0000-4000-8000-000000000003","message":{"role":"assistant","content":[{"type":"text","text":"per memory aaaaaaaaaaaa, doing the thing now."}]}}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert!(
            derived.rows[0].used,
            "id referenced in a later turn must count"
        );
    }

    /// A later turn naming the memory's TITLE verbatim (no id reference at
    /// all) also marks the hit `used`, when the title clears
    /// `MIN_TITLE_LEN_FOR_REFERENCE_MATCH`.
    #[test]
    fn derive_memory_recalls_marks_used_when_title_referenced_later() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- database backoff policy  [notes]  (id bbbbbbbbbbbb)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"23000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"used-test-2"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"23000002-0000-4000-8000-000000000002","parentUuid":"23000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:06.000Z","sessionId":"used-test-2"}"#,
            r#"{"type":"assistant","timestamp":"2026-06-04T09:00:07.000Z","uuid":"23000003-0000-4000-8000-000000000003","message":{"role":"assistant","content":[{"type":"text","text":"applying the database backoff policy here."}]}}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert!(
            derived.rows[0].used,
            "title referenced (no id) in a later turn must count"
        );
    }

    /// A title under the collision-prone floor never triggers a title
    /// match, even when the exact words appear in a later turn — the id
    /// isn't mentioned either, so the hit stays unused.
    #[test]
    fn derive_memory_recalls_skips_title_match_under_the_length_floor() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- fix bug  [notes]  (id cccccccccccc)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"24000011-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"used-test-3"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"24000012-0000-4000-8000-000000000002","parentUuid":"24000011-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:06.000Z","sessionId":"used-test-3"}"#,
            r#"{"type":"assistant","timestamp":"2026-06-04T09:00:07.000Z","uuid":"24000013-0000-4000-8000-000000000003","message":{"role":"assistant","content":[{"type":"text","text":"yep, fix bug done."}]}}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert_eq!("fix bug".chars().count(), 7);
        assert!(
            !derived.rows[0].used,
            "a sub-floor title must never drive a match"
        );
    }

    /// No later turn names the memory at all — `used` stays false.
    #[test]
    fn derive_memory_recalls_used_false_when_never_referenced() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- an entirely unrelated memory title  [notes]  (id dddddddddddd)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"25000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"used-test-4"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"25000002-0000-4000-8000-000000000002","parentUuid":"25000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:06.000Z","sessionId":"used-test-4"}"#,
            r#"{"type":"assistant","timestamp":"2026-06-04T09:00:07.000Z","uuid":"25000003-0000-4000-8000-000000000003","message":{"role":"assistant","content":[{"type":"text","text":"working on something else entirely."}]}}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert!(!derived.rows[0].used);
    }

    /// A turn BEFORE the injection already contains the memory's title —
    /// that must NOT count as a reference (it's what got the memory
    /// recalled in the first place, or simple coincidence, never a
    /// response to the injection). Only turns strictly AFTER the
    /// injection's own turn are scanned.
    #[test]
    fn derive_memory_recalls_ignores_a_reference_before_the_injection_turn() {
        let jsonl = [
            // Turn 1 (Human, BEFORE the injection): mentions the exact
            // phrase that will later become the recalled memory's title.
            r#"{"type":"user","timestamp":"2026-06-04T09:00:00.000Z","uuid":"26000001-0000-4000-8000-000000000001","message":{"role":"user","content":"please remember the retry cap policy text later"}}"#,
            // The injection, folded into the NEXT turn (turn 2).
            r#"{"parentUuid":"26000001-0000-4000-8000-000000000001","isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- retry cap policy text  [notes]  (id eeeeeeeeeeee)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"26000002-0000-4000-8000-000000000002","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"used-test-5"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"continue"},"uuid":"26000003-0000-4000-8000-000000000003","parentUuid":"26000002-0000-4000-8000-000000000002","timestamp":"2026-06-04T09:00:06.000Z","sessionId":"used-test-5"}"#,
            // No turn follows the injection's own turn.
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert!(
            !derived.rows[0].used,
            "a pre-injection mention must never count as a reference"
        );
    }

    /// CT-C1 × CT-C5 integration (orchestrator-added): a hit the hook
    /// rendered with the `⚠ disputed:` flag prefix still title-matches on
    /// its PLAIN title — `parse_recall_item_parts` strips the prefix, so a
    /// later turn naming the bare title marks the hit used.
    #[test]
    fn derive_memory_recalls_title_match_survives_the_disputed_prefix() {
        let jsonl = [
            r#"{"parentUuid":null,"isSidechain":false,"attachment":{"type":"hook_additional_context","content":["Relevant memories from kb (recall - these persist across sessions):\n- ⚠ disputed: database backoff policy  [notes]  (id ffffffffffff)"],"hookName":"UserPromptSubmit","toolUseID":"UserPromptSubmit","hookEvent":"UserPromptSubmit"},"type":"attachment","uuid":"27000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:05.000Z","sessionId":"used-test-6"}"#,
            r#"{"isSidechain":false,"type":"user","message":{"role":"user","content":"go"},"uuid":"27000002-0000-4000-8000-000000000002","parentUuid":"27000001-0000-4000-8000-000000000001","timestamp":"2026-06-04T09:00:06.000Z","sessionId":"used-test-6"}"#,
            r#"{"type":"assistant","timestamp":"2026-06-04T09:00:07.000Z","uuid":"27000003-0000-4000-8000-000000000003","message":{"role":"assistant","content":[{"type":"text","text":"applying the database backoff policy here."}]}}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let derived = derive_memory_recalls(&v);
        assert_eq!(derived.rows.len(), 1);
        assert!(
            derived.rows[0].used,
            "the disputed prefix must not defeat a plain-title reference"
        );
    }

    #[test]
    fn kb_command_bash_call_becomes_a_kb_command_card() {
        let v = view("synthetic-kb-commands");
        let kb_items: Vec<&Item> = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .filter(|i| matches!(i, Item::KbCommand { .. }))
            .collect();
        // recall, search, remember, recollect — 4 kb-verb Bash calls.
        assert_eq!(kb_items.len(), 4);
        let verbs: Vec<&str> = kb_items
            .iter()
            .map(|i| match i {
                Item::KbCommand { verb, .. } => verb.as_str(),
                _ => unreachable!(),
            })
            .collect();
        assert!(verbs.contains(&"recall"));
        assert!(verbs.contains(&"remember"));
        let remember = kb_items
            .iter()
            .find(|i| matches!(i, Item::KbCommand { verb, .. } if verb == "remember"))
            .unwrap();
        let Item::KbCommand { result_view, .. } = remember else {
            unreachable!()
        };
        assert!(result_view.as_ref().unwrap().contains("remembered"));
    }

    #[test]
    fn husk_fixture_never_panics_and_has_no_outcome() {
        let v = view("synthetic-husk");
        assert!(v.header.outcome.is_none());
        assert!(v.outline.is_empty());
        assert!(v.tasks_final.is_empty());
        // The /clear command chip still renders (never worse than today).
        assert!(v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .any(|i| matches!(i, Item::Command { name, .. } if name.contains("clear"))));
    }

    // --- AskUserQuestion / plan / permission decisions ----------------------

    #[test]
    fn structured_answers_become_one_decision_item_per_question() {
        let jsonl = [
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"AskUserQuestion","input":{}}]}}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:10Z","toolUseResult":{"answers":{"Pick one":"Option B"},"questions":[{"question":"Pick one"}]},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"Your questions have been answered: \"Pick one\"=\"Option B\"."}]}}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let decisions: Vec<&Item> = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .filter(|i| matches!(i, Item::Decision { .. }))
            .collect();
        assert_eq!(decisions.len(), 1);
        assert!(matches!(decisions[0], Item::Decision { prompt, answer }
            if prompt == "Pick one" && answer.as_deref() == Some("Option B")));
        // No leftover generic ToolCall for the same call.
        assert!(!v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .any(|i| matches!(i, Item::ToolCall { name, .. } if name == "AskUserQuestion")));
    }

    #[test]
    fn plan_approval_and_permission_denial_become_decisions() {
        let jsonl = [
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"ExitPlanMode","input":{}}]}}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:00:10Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"User has approved your plan. You can now start coding."}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-01-01T00:01:00Z","uuid":"22222222-0000-4000-8000-000000000002","requestId":"r2","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu2","name":"Bash","input":{"command":"rm -rf /"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-01-01T00:01:10Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu2","is_error":true,"content":"Permission for this action was denied by the classifier. Reason: too dangerous."}]}}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let decisions: Vec<&Item> = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .filter(|i| matches!(i, Item::Decision { .. }))
            .collect();
        assert_eq!(decisions.len(), 2);
        assert!(decisions
            .iter()
            .any(|d| matches!(d, Item::Decision { prompt, .. } if prompt == "plan approved")));
        assert!(decisions.iter().any(|d| matches!(d,
            Item::Decision { prompt, answer }
                if prompt == "permission denied" && answer.as_deref() == Some("too dangerous."))));
    }

    // --- closure / outcome (R3) --------------------------------------------

    #[test]
    fn outcome_matches_closing_assistant_text_and_carries_commits() {
        let jsonl = fixture("synthetic-workflow-heavy");
        let expected = super::super::closing_assistant_text(&jsonl).unwrap();
        let commits = super::super::extract_commits_block; // exists, unused directly here
        let _ = commits;
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let outcome = v.header.outcome.expect("outcome present");
        assert_eq!(outcome.text, expected);
        assert!(outcome.turn_id.is_some());
    }

    /// PVIEW-2 — turn-derived closure must stay byte-identical to the
    /// standalone JSONL scanner across every view fixture.
    #[test]
    fn outcome_matches_closing_assistant_text_across_all_fixtures() {
        for name in [
            "synthetic-workflow-heavy",
            "synthetic-kb-commands",
            "synthetic-husk",
        ] {
            let jsonl = fixture(name);
            let expected = super::super::closing_assistant_text(&jsonl);
            let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
            let got = v.header.outcome.as_ref().map(|o| o.text.as_str());
            assert_eq!(
                got,
                expected.as_deref(),
                "{name}: turn-derived outcome must equal closing_assistant_text"
            );
        }
    }

    /// QERR-1 — a Workflow script whose multi-byte char straddles the 64 KiB
    /// scan cap must not panic; meta fields still resolve from the head.
    #[test]
    fn parse_workflow_meta_tolerates_multibyte_at_scan_cap() {
        // Build a script whose UTF-8 length is > WORKFLOW_SCAN_CAP_BYTES with
        // a multi-byte char (€ = 3 bytes) straddling the 65536 boundary, and
        // with meta fields in the head so a successful non-panic scan still
        // returns them.
        // `scan_phase_entries` matches the `phase: '<label>'` shape (a
        // progress-line scan, not the meta array) — fixture uses that form.
        let mut script =
            String::from("let meta = { name: 'cap-test', description: 'boundary' };\nphase: 'a'\n");
        // Pad with ASCII until we're a few bytes short of the cap, then a
        // run of € so at least one straddles byte 65536.
        while script.len() < WORKFLOW_SCAN_CAP_BYTES - 2 {
            script.push('x');
        }
        for _ in 0..8 {
            script.push('€'); // 3-byte UTF-8
        }
        assert!(script.len() > WORKFLOW_SCAN_CAP_BYTES);
        // Confirm the cap index is mid-character for at least one of the €s.
        assert!(!script.is_char_boundary(WORKFLOW_SCAN_CAP_BYTES));

        let input = serde_json::json!({ "script": script });
        let (name, description, phases) = parse_workflow_meta(&input);
        assert_eq!(name.as_deref(), Some("cap-test"));
        assert_eq!(description.as_deref(), Some("boundary"));
        assert_eq!(phases, vec!["a".to_string()]);
    }

    /// QTEST-10 — turn ids for a shared prefix stay stable when new turns
    /// are appended (uuid-derived ids don't renumber).
    #[test]
    fn turn_ids_stable_when_new_turns_appended() {
        let prefix = concat!(
            r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","uuid":"aaaaaaaa-0000-4000-8000-000000000001","message":{"role":"user","content":"hello"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","uuid":"bbbbbbbb-0000-4000-8000-000000000002","message":{"role":"assistant","content":[{"type":"text","text":"hi there, this is a substantial enough closing line for the threshold."}]}}"#,
            "\n",
        );
        let suffix = concat!(
            r#"{"type":"user","timestamp":"2026-01-01T00:00:02Z","uuid":"cccccccc-0000-4000-8000-000000000003","message":{"role":"user","content":"more"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:03Z","uuid":"dddddddd-0000-4000-8000-000000000004","message":{"role":"assistant","content":[{"type":"text","text":"and more prose after the prefix."}]}}"#,
            "\n",
        );
        let base = session_view(prefix, &TailBlocks::default(), &ViewOptions::default());
        let extended = session_view(
            &format!("{prefix}{suffix}"),
            &TailBlocks::default(),
            &ViewOptions::default(),
        );
        assert!(
            extended.turns.len() > base.turns.len(),
            "extended must grow"
        );
        for (b, e) in base.turns.iter().zip(extended.turns.iter()) {
            assert_eq!(b.id, e.id, "prefix turn id drifted: {} vs {}", b.id, e.id);
            assert_eq!(b.ordinal, e.ordinal);
            assert_eq!(b.role, e.role);
        }
    }

    #[test]
    fn parse_turn_window_accepts_single_and_range_forms() {
        assert_eq!(parse_turn_window("5"), Some((5, 5)));
        assert_eq!(parse_turn_window("5..7"), Some((5, 7)));
        assert_eq!(parse_turn_window("7..5"), Some((5, 7)));
        assert_eq!(parse_turn_window(""), None);
        assert_eq!(parse_turn_window("x..y"), None);
        assert_eq!(parse_turn_window("5.."), None);
        assert_eq!(parse_turn_window("  3 .. 3  "), Some((3, 3)));
    }

    // --- turn identity (R2) -------------------------------------------------

    #[test]
    fn turn_ids_are_t_prefixed_12_hex_and_stable_across_repeated_calls() {
        let jsonl = fixture("synthetic-kb-commands");
        let a = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let b = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        assert_eq!(
            a.turns.iter().map(|t| &t.id).collect::<Vec<_>>(),
            b.turns.iter().map(|t| &t.id).collect::<Vec<_>>()
        );
        for t in &a.turns {
            assert!(t.id.starts_with("t-"), "{}", t.id);
            let hex = &t.id[2..];
            assert_eq!(hex.len(), 12, "{}", t.id);
            assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "{}", t.id);
        }
    }

    #[test]
    fn outline_rows_carry_both_ordinal_and_stable_id() {
        let v = view("synthetic-kb-commands");
        assert!(!v.outline.is_empty());
        for row in &v.outline {
            assert!(row.n > 0);
            assert!(row.id.starts_with("t-"));
        }
    }

    // --- empty thinking / ghost elimination (hygiene bundle) ---------------

    #[test]
    fn empty_thinking_is_flagged_empty_never_dropped() {
        let v = view("synthetic-workflow-heavy");
        assert!(v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .any(|i| matches!(i, Item::Thinking { empty: true, .. })));
        assert!(v.stats.thinking_empty > 0);
    }

    #[test]
    fn no_ghost_turns_every_turn_has_at_least_one_item() {
        for name in [
            "synthetic-workflow-heavy",
            "synthetic-kb-commands",
            "synthetic-husk",
        ] {
            let v = view(name);
            for t in &v.turns {
                assert!(!t.items.is_empty(), "{name}: ghost turn {}", t.id);
            }
        }
    }

    // --- minimap determinism (Proposal 8) -----------------------------------

    #[test]
    fn minimap_is_byte_deterministic_across_repeated_builds() {
        let jsonl = fixture("synthetic-workflow-heavy");
        let a = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let b = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let pa: Vec<(u32, u32)> = a.minimap.iter().map(|p| (p.ordinal, p.pos_1000)).collect();
        let pb: Vec<(u32, u32)> = b.minimap.iter().map(|p| (p.ordinal, p.pos_1000)).collect();
        assert_eq!(pa, pb);
        assert!(!a.minimap.is_empty());
        for p in &a.minimap {
            assert!(p.pos_1000 <= 1000);
        }
    }

    // --- XSS-adjacent: the engine stores text verbatim; escaping is the
    // presenter's job, but a raw `<script>` in a task subject must survive
    // the join pass unmangled so the presenter's esc() can do its job.
    #[test]
    fn script_tag_in_task_subject_survives_the_join_pass_unmangled() {
        let jsonl = [
            r#"{"type":"assistant","timestamp":"2026-01-01T00:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"tu1","name":"TaskCreate","input":{"subject":"<script>alert(1)</script>"}}]}}"#,
        ].join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let headline = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .find_map(|i| match i {
                Item::ToolCall { name, headline, .. } if name == "TaskCreate" => {
                    Some(headline.clone())
                }
                _ => None,
            })
            .expect("TaskCreate item present");
        assert!(headline.contains("<script>"));
    }

    // --- kb-command invocation parser ---------------------------------------

    #[test]
    fn kb_cli_invocation_parses_env_and_cd_prefixed_forms() {
        assert_eq!(
            kb_cli_invocation(r#"kb recall "foo""#),
            Some(("recall".to_string(), r#""foo""#.to_string()))
        );
        assert_eq!(
            kb_cli_invocation("cd /tmp && kb search bar"),
            Some(("search".to_string(), "bar".to_string()))
        );
        assert_eq!(kb_cli_invocation("echo hi"), None);
    }

    // --- the incremental constructor + THE EQUIVALENCE GOLDEN ---------------

    /// Fold [`view_append`] over `lines` chunked into groups of `chunk_size`
    /// lines, finishing with [`view_finish`] — the exact contract
    /// [`session_view`] itself relies on internally.
    fn fold_incremental(jsonl: &str, chunk_size: usize) -> Vec<Turn> {
        let lines: Vec<&str> = jsonl.lines().collect();
        let mut carry = ViewCarry::default();
        let mut turns = Vec::new();
        for chunk in lines.chunks(chunk_size.max(1)) {
            let joined = chunk.join("\n");
            let (events, c2) = view_append(carry, &joined);
            carry = c2;
            for e in events {
                if let ViewEvent::TurnClosed(t) = e {
                    turns.push(t);
                }
            }
        }
        for e in view_finish(&mut carry) {
            if let ViewEvent::TurnClosed(t) = e {
                turns.push(t);
            }
        }
        turns
    }

    /// A turn's structural fingerprint for cross-chunking comparison — id,
    /// role, item KIND sequence (not full item payloads, since a
    /// `ToolCall.raw` result JSON's whitespace is not itself part of the
    /// equivalence claim — see the module docs' "emitted-kind subset" note),
    /// plus `raw_lines` (the F1 follow-up field: the decoded-line indices
    /// backing the turn must not depend on chunking either — `--turn N
    /// --raw` slices by them).
    fn fingerprint(t: &Turn) -> (String, &'static str, Vec<&'static str>, Vec<u32>) {
        let role = match t.role {
            Role::Human => "human",
            Role::Assistant => "assistant",
        };
        let kinds = t
            .items
            .iter()
            .map(|i| match i {
                Item::Prose { .. } => "prose",
                Item::Thinking { .. } => "thinking",
                Item::ToolCall { .. } => "tool_call",
                Item::Command { .. } => "command",
                Item::TaskEvent { .. } => "task_event",
                Item::WorkflowCard { .. } => "workflow_card",
                Item::KbCommand { .. } => "kb_command",
                Item::MemoryInjection { .. } => "memory_injection",
                Item::SystemReminder { .. } => "system_reminder",
                Item::Decision { .. } => "decision",
                Item::ModeChange { .. } => "mode_change",
                Item::TimeGap { .. } => "time_gap",
                Item::Raw { .. } => "raw",
            })
            .collect();
        (t.id.clone(), role, kinds, t.raw_lines.clone())
    }

    #[test]
    fn equivalence_golden_across_chunk_sizes() {
        for fixture_name in [
            "synthetic-workflow-heavy",
            "synthetic-kb-commands",
            "synthetic-husk",
        ] {
            let jsonl = fixture(fixture_name);
            let full = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
            let full_fp: Vec<_> = full.turns.iter().map(fingerprint).collect();

            for chunk_size in [1usize, 2, 5, 10_000] {
                let incremental = fold_incremental(&jsonl, chunk_size);
                let inc_fp: Vec<_> = incremental.iter().map(fingerprint).collect();
                assert_eq!(
                    full_fp, inc_fp,
                    "{fixture_name} @ chunk_size={chunk_size}: view_full must equal \
                     fold(view_append) for the turn-id/role/item-kind subset"
                );
            }
        }
    }

    #[test]
    fn view_bootstrap_never_panics_on_a_mid_stream_window() {
        let jsonl = fixture("synthetic-workflow-heavy");
        let lines: Vec<&str> = jsonl.lines().collect();
        let window = lines[10..].join("\n");
        let mut carry = view_bootstrap(&window);
        // Finishing a bootstrapped carry must not panic even though it never
        // saw the document's opening lines.
        let _ = view_finish(&mut carry);
    }

    // --- W6 — bare SGR remnant strip (W4 builder report, P12 tail) --------

    /// FAILING-FIRST PROOF: `\x1b[1m\x1b[32mok\x1b[0m` survives the capture
    /// pipeline's ESC-byte strip as literal `[1m[32mok[0m` — before this
    /// fix, `strip_stdout_wrapper` returned that string VERBATIM (only
    /// unwrapping the `<local-command-stdout>` tag), so every presenter
    /// (renderer/CLI/wire) showed the raw ANSI numbers inline. The regex
    /// strip closes that.
    #[test]
    fn bare_sgr_remnants_are_stripped_from_command_stdout() {
        let jsonl = [
            r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"<command-name>/status</command-name>"}}"#,
            r#"{"type":"user","timestamp":"2026-05-27T08:00:05Z","message":{"role":"user","content":"<local-command-stdout>[1m[32mAll green[0m — 3 checks passed[22m</local-command-stdout>"}}"#,
        ]
        .join("\n");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let stdout = v
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .find_map(|i| match i {
                Item::Command {
                    stdout: Some(s), ..
                } => Some(s.clone()),
                _ => None,
            })
            .expect("command turn with stdout");
        assert_eq!(stdout, "All green — 3 checks passed");
        assert!(!stdout.contains('['), "{stdout}");
    }

    /// Goldens: a multi-code SGR sequence (`[38;5;208m` truecolor shape)
    /// strips fully; ordinary prose brackets that merely LOOK bracket-like
    /// but don't end in a bare `m` after digits survive unchanged (the
    /// conservative-shape requirement — never eat real prose brackets).
    #[test]
    fn sgr_strip_is_conservative_about_what_it_eats() {
        assert_eq!(
            strip_bare_sgr_remnants("[38;5;208mwarn[0m: check [1] and [TODO]"),
            "warn: check [1] and [TODO]"
        );
        // No digits immediately before `m`, or no trailing `m` at all — a
        // real prose bracket must round-trip byte-for-byte.
        let untouched = "see [note] below, or item [1], or status [Ok]";
        assert_eq!(strip_bare_sgr_remnants(untouched), untouched);
    }

    // --- W6 — memory-session comment-anchor resolution (moonshots M2 / memo R2) --

    /// FAILING-FIRST PROOF: before this fix, EVERY anchor (session or not)
    /// resolved via `review::fuzzy_resolve_anchor(html, anchor)` against the
    /// raw capture bytes. Feed that generic resolver a `Section{id:
    /// "<a real t-<uuid12> turn id>"}` anchor against this fixture's raw
    /// `<pre>{escaped JSONL}</pre>` HTML and it returns `Stale` — the id
    /// exists nowhere in those bytes (the renderer mints it at serve time).
    /// `resolve_capture_anchor` against the INTERPRETED view is the fix:
    /// same anchor, same document, now `Exact`.
    #[test]
    fn section_turn_anchor_resolves_exact_against_the_view_but_stale_against_raw_html() {
        let jsonl = fixture("synthetic-workflow-heavy");
        let v = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());
        let turn_id = v.turns.first().expect("fixture has turns").id.clone();
        assert!(turn_id.starts_with("t-"));

        let anchor = review::Anchor::Section {
            id: turn_id.clone(),
            tag: None,
            snippet: None,
        };

        // THE FIX: resolves against the interpreted view.
        assert_eq!(
            resolve_capture_anchor(&v, "<html></html>", &anchor),
            review::Resolution::Exact(turn_id.clone())
        );

        // THE VERIFIED GAP (still true, proving the fix is load-bearing, not
        // redundant): the id is byte-absent from a raw capture-shaped
        // document, so the GENERIC resolver still calls it Stale.
        let raw_capture_html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><h1>t</h1><pre>{}</pre></body></html>",
            html_escape_for_pre(&jsonl)
        );
        assert_eq!(
            review::fuzzy_resolve_anchor(&raw_capture_html, &anchor),
            review::Resolution::Stale,
            "the raw-HTML resolver must still be blind to t-<uuid12> ids — \
             that's the bug this module's category-gated branch works around"
        );
    }

    /// The outcome footer's `#ses-outcome` id resolves Exact too — it's a
    /// static section the renderer always emits once any turns exist (not a
    /// per-turn id), so it's checked separately from the turn-id ladder.
    #[test]
    fn section_ses_outcome_anchor_resolves_exact() {
        let v = view("synthetic-workflow-heavy");
        let anchor = review::Anchor::Section {
            id: crate::sessions::SES_OUTCOME_ANCHOR.to_string(),
            tag: None,
            snippet: None,
        };
        assert_eq!(
            resolve_capture_anchor(&v, "<html></html>", &anchor),
            review::Resolution::Exact(crate::sessions::SES_OUTCOME_ANCHOR.to_string())
        );
    }

    /// D5 (memo/moonshots M2): a pre-milestone ordinal `turn-N` anchor (or
    /// any id that isn't a live turn/outcome id) is the ACCEPTED, documented
    /// break — must resolve Stale, not silently rebind to the wrong turn.
    #[test]
    fn section_legacy_ordinal_anchor_resolves_stale_not_silently_rebound() {
        let v = view("synthetic-workflow-heavy");
        let anchor = review::Anchor::Section {
            id: "turn-1".to_string(),
            tag: None,
            snippet: None,
        };
        assert_eq!(
            resolve_capture_anchor(&v, "<html></html>", &anchor),
            review::Resolution::Stale
        );
    }

    /// FAILING-FIRST PROOF for Selection scope: the header's opening quote
    /// (real rendered prose) is matched Exact against the interpreted view,
    /// but is byte-absent from the raw escaped `<pre>` (the JSONL wraps it
    /// inside a `content` string field, not as bare paragraph text) — so the
    /// generic block-level DOM resolver (`p, li, blockquote, td, h1-h6`)
    /// finds no matching block there at all and would also call it Stale in
    /// practice (a capture's raw HTML has no `<p>` elements).
    #[test]
    fn selection_anchor_on_rendered_prose_resolves_exact_against_the_view() {
        let v = view("synthetic-workflow-heavy");
        let opening = v
            .header
            .opening
            .as_ref()
            .expect("fixture has an opening quote")
            .text
            .clone();
        assert!(opening.starts_with("Build the atlas fit workflow"));

        let anchor = review::Anchor::Selection {
            css_path: "body > main:nth-of-type(1) > p:nth-of-type(1)".to_string(),
            offset: 0,
            snippet: opening.clone(),
        };
        assert_eq!(
            resolve_capture_anchor(&v, "<html></html>", &anchor),
            review::Resolution::Exact(opening)
        );
    }

    /// A snippet that matches nothing in the transcript resolves Stale (the
    /// resolver doesn't hallucinate a match).
    #[test]
    fn selection_anchor_with_no_match_resolves_stale() {
        let v = view("synthetic-workflow-heavy");
        let anchor = review::Anchor::Selection {
            css_path: String::new(),
            offset: 0,
            snippet: "this text never appears anywhere in the fixture transcript, at all"
                .to_string(),
        };
        assert_eq!(
            resolve_capture_anchor(&v, "<html></html>", &anchor),
            review::Resolution::Stale
        );
    }

    /// File/Chapter anchors are out of this fix's scope (memo R2) and must
    /// keep falling through to the ordinary byte-level resolver — pinning
    /// that `resolve_capture_anchor` doesn't quietly swallow them too.
    #[test]
    fn file_anchor_still_uses_the_generic_resolver() {
        let v = view("synthetic-workflow-heavy");
        assert_eq!(
            resolve_capture_anchor(&v, "<html></html>", &review::Anchor::File),
            review::Resolution::Exact("file".to_string())
        );
    }

    /// `session_view_for_capture_html` is the indexer-facing entry point —
    /// prove it round-trips a REAL capture-shaped document (escaped `<pre>`
    /// and all, not the bare-JSONL `view()` test helper) to the same turn
    /// ids `session_view` would produce directly from the decoded JSONL.
    #[test]
    fn session_view_for_capture_html_decodes_the_pre_block() {
        let jsonl = fixture("synthetic-workflow-heavy");
        let direct = session_view(&jsonl, &TailBlocks::default(), &ViewOptions::default());

        let capture_html = format!(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><h1>t</h1><pre>{}</pre></body></html>",
            html_escape_for_pre(&jsonl)
        );
        let via_capture = session_view_for_capture_html(&capture_html);

        let direct_ids: Vec<&str> = direct.turns.iter().map(|t| t.id.as_str()).collect();
        let capture_ids: Vec<&str> = via_capture.turns.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(direct_ids, capture_ids);
    }

    /// A hand-edited/truncated capture with no `<pre>` at all must yield an
    /// EMPTY view (zero turns), not panic — every anchor against it then
    /// correctly resolves Stale via the ordinary `id`/text-absent path.
    #[test]
    fn session_view_for_capture_html_tolerates_a_pre_less_document() {
        let v = session_view_for_capture_html(
            "<html><head><meta name=\"kb-category\" content=\"memory-session\"></head>\
             <body><h1>no pre here</h1></body></html>",
        );
        assert!(v.turns.is_empty());
    }

    /// Minimal 3-entity escape mirroring `kb-capture.sh`'s `sed` pipeline
    /// (`&`→`&amp;` FIRST, then `<`/`>`) — matches `recover_jsonl_from_
    /// capture`'s inverse, just enough for these tests' synthetic fixtures.
    fn html_escape_for_pre(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
}
