//! `session-replay/1` — the pure session-replay timeline extractor.
//!
//! A session capture already carries everything needed to *replay* the work
//! beat by beat: the byte-identical `<pre>` transcript (recovered by
//! [`recover_jsonl_from_capture`](super::recover_jsonl_from_capture)) has an
//! ISO timestamp on every `user` / `assistant` record. The sqlite side does
//! NOT — `session_decisions` / `session_commits` / `session_research` carry
//! only a `seq`, `session_files` carries neither, and `sessions` has only
//! `started_at` / `ended_at`. So the clock lives here, derived from the
//! transcript on demand.
//!
//! [`replay_timeline`] turns that JSONL into an ordered list of
//! [`ReplayBeat`]s — one per narrative moment (a prompt, a tool call, a
//! commit, a steering decision). It is **pure**: no clock (never
//! `SystemTime::now`), no I/O, no corpus knowledge, no LLM. The same bytes
//! always yield the same timeline, which is what makes it safe to golden-pin
//! (see the tests below) and to share verbatim between the CLI and the SPA.
//!
//! ### Design rulings (recorded so a later reader doesn't re-litigate them)
//!
//! * **Transcript order is preserved; we never sort.** The JSONL order IS the
//!   causal order — a record's position is ground truth in a way its
//!   `timestamp` is not (clock skew across a resumed session, a sidechain
//!   flushed late). Sorting by timestamp would reorder cause and effect. We
//!   therefore keep the file order and, when a timestamp goes *backwards*,
//!   clamp only the derived [`ReplayBeat::delta_secs`] to `0` (so a playhead
//!   clock stays monotonic) while leaving [`ReplayBeat::ts_unix`] raw so a
//!   consumer can still see the inversion. Inversions are counted in
//!   [`ReplayTimeline::out_of_order`].
//! * **Timestamps come from [`crate::timeparse::parse_iso_utc`]** — the same
//!   parser [`parse_session_activity`](super::parse_session_activity) uses for
//!   `ended_at`, so the playhead clock and the session row can never disagree.
//! * **Timestamp-less records are metadata** (last-prompt, mode,
//!   permission-mode, ai-title, file-history-snapshot). They produce no beat;
//!   they are counted in [`ReplayTimeline::metadata_skipped`] rather than
//!   silently dropped.
//! * **Runs collapse.** A transcript that reads one file 400 times must not
//!   yield 400 beats — see [`REPLAY_COLLAPSE_WINDOW_SECS`].
//! * **Truncation is reported, never silent** —
//!   [`ReplayTimeline::truncated`] + [`ReplayTimeline::dropped`].

use serde::{Deserialize, Serialize};

use super::{git_action_of, is_wrapper_text, truncate_chars, user_message_text, Decision};
use crate::timeparse::parse_iso_utc;

/// Grammar tag for the wire shape produced by [`replay_timeline`]. Bump the
/// suffix (never the meaning of an existing one) if a beat field changes
/// incompatibly, exactly as `kb-comments/1` / `kb-list/1` do.
pub const REPLAY_GRAMMAR: &str = "session-replay/1";

/// Hard SAFETY ceiling on the number of beats a single timeline may carry —
/// NOT a serve window (memo R7/S6/D4: "compute full then slice"). Raised
/// 2,000 → 50,000 this wave: the route now serves a windowed slice
/// (`?from_seq=&limit=`) over the FULL computed timeline, so the cap only
/// ever bites a truly pathological transcript (tens of thousands of beats);
/// for every real session `replay_timeline` now genuinely computes the
/// complete beat list, killing the old "the session tail vanishes past 2,000
/// beats" defect (S6). When the cap DOES bite, [`ReplayTimeline::truncated`]
/// is set and every skipped beat is counted in [`ReplayTimeline::dropped`] —
/// honest truncation, never a silent cut. Collapsing into the tail run still
/// works past the cap (it doesn't grow the vec), so the last beat's `count`
/// stays truthful.
pub const REPLAY_MAX_EVENTS: usize = 50_000;

/// Consecutive beats of the same kind on the same path within this many
/// seconds fold into ONE beat carrying a `count`. Two minutes is chosen from
/// the shape of real transcripts: an agent turn fires its tool calls
/// back-to-back within seconds, so a genuine "run" (400 reads of one file
/// while grepping through it) always lands inside the window, while a return
/// to the same file after thinking/asking reads as a new beat.
pub const REPLAY_COLLAPSE_WINDOW_SECS: i64 = 120;

/// Cap on [`ReplayBeat::detail`] (a one-line label: a basename, a command, a
/// question). Chars, not bytes — the label is rendered, not stored.
pub const REPLAY_DETAIL_MAX_CHARS: usize = 120;

/// Cap on [`ReplayBeat::snippet`] (the edited text region). Big enough to
/// recognise the hunk, small enough that a 2 000-beat timeline stays a
/// reasonable payload.
pub const REPLAY_SNIPPET_MAX_CHARS: usize = 240;

/// What one beat of a replay *is*. A small closed set, chosen from what the
/// transcript actually supports — every variant below is derived from a field
/// that is genuinely present in Claude Code JSONL:
///
/// * [`Prompt`](ReplayKind::Prompt) — a human turn (`role:"user"` with real
///   text, not a tool_result and not a synthetic wrapper).
/// * [`Assistant`](ReplayKind::Assistant) — an assistant `text` block (the
///   agent narrating). `thinking` blocks are NOT beats.
/// * [`Read`](ReplayKind::Read) / [`Write`](ReplayKind::Write) /
///   [`Edit`](ReplayKind::Edit) — the file-touching tools, mapped exactly as
///   [`super::FileAction`] maps them so the replay and `session_files` can
///   never disagree about what a tool name means.
/// * [`Bash`](ReplayKind::Bash) — a shell command that is not a VCS action.
/// * [`Commit`](ReplayKind::Commit) — a Bash command `git_action_of`
///   recognises (commit / push / tag). Emitted at the moment of the *call*
///   (the assistant record), which is where the clock is; the SHA lands later
///   in the tool_result and is deliberately not part of a beat.
/// * [`Decision`](ReplayKind::Decision) — an AskUserQuestion answer or plan
///   approval, extracted by the same `extract_decisions_and_errors` the
///   decisions log uses.
/// * [`Search`](ReplayKind::Search) — Grep / Glob / WebSearch / WebFetch.
/// * [`Subagent`](ReplayKind::Subagent) — a Task/Agent delegation.
/// * [`Outcome`](ReplayKind::Outcome) — R3/R7/S6: the session's CLOSURE beat
///   (the assistant record matching [`super::closing_assistant_text`]'s
///   selection) — an [`Assistant`](ReplayKind::Assistant) beat promoted to
///   its own kind so the timeline's last real prose reads as the ending
///   moment, not just one more narration beat.
/// * [`Other`](ReplayKind::Other) — any remaining `tool_use` (Skill, MCP
///   tools, TodoWrite, …), labelled with its tool name rather than dropped.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayKind {
    Prompt,
    Assistant,
    Read,
    Edit,
    Write,
    Bash,
    Commit,
    Decision,
    Search,
    Subagent,
    Outcome,
    Other,
}

impl ReplayKind {
    /// Lowercase wire token — the value that rides HTTP/JSON and the CLI.
    pub fn as_str(self) -> &'static str {
        match self {
            ReplayKind::Prompt => "prompt",
            ReplayKind::Assistant => "assistant",
            ReplayKind::Read => "read",
            ReplayKind::Edit => "edit",
            ReplayKind::Write => "write",
            ReplayKind::Bash => "bash",
            ReplayKind::Commit => "commit",
            ReplayKind::Decision => "decision",
            ReplayKind::Search => "search",
            ReplayKind::Subagent => "subagent",
            ReplayKind::Outcome => "outcome",
            ReplayKind::Other => "other",
        }
    }

    /// Parse the wire token back. `None` on an unknown value (forward-compat
    /// with a kind a future grammar adds).
    pub fn from_wire(s: &str) -> Option<Self> {
        Some(match s {
            "prompt" => ReplayKind::Prompt,
            "assistant" => ReplayKind::Assistant,
            "read" => ReplayKind::Read,
            "edit" => ReplayKind::Edit,
            "write" => ReplayKind::Write,
            "bash" => ReplayKind::Bash,
            "commit" => ReplayKind::Commit,
            "decision" => ReplayKind::Decision,
            "search" => ReplayKind::Search,
            "subagent" => ReplayKind::Subagent,
            "outcome" => ReplayKind::Outcome,
            "other" => ReplayKind::Other,
            _ => return None,
        })
    }

    /// Whether a run of this kind may fold into one beat with a `count`.
    /// Narrative kinds (prompt / assistant / decision / commit / subagent)
    /// never collapse: each is a distinct moment whose text differs, and
    /// folding them would destroy the story the replay exists to tell.
    fn collapsible(self) -> bool {
        matches!(
            self,
            ReplayKind::Read
                | ReplayKind::Edit
                | ReplayKind::Write
                | ReplayKind::Bash
                | ReplayKind::Search
                | ReplayKind::Other
        )
    }
}

/// One beat of a session replay.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayBeat {
    /// Position in the timeline, 0-based and dense (a collapsed run occupies
    /// ONE seq). Stable for a given input — the SPA keys on it.
    pub seq: usize,
    /// Unix seconds from the record's ISO `timestamp`, verbatim (never
    /// clamped) — an out-of-order transcript keeps its real instants.
    pub ts_unix: i64,
    /// Seconds since the previous beat; `0` for the first beat and clamped to
    /// `0` when the transcript's clock goes backwards (see the module docs).
    pub delta_secs: i64,
    pub kind: ReplayKind,
    /// Short deterministic label — a path basename, a command, a question, a
    /// commit subject. Truncated to [`REPLAY_DETAIL_MAX_CHARS`].
    pub detail: String,
    /// The raw path from the tool call, verbatim (absolute OR relative — the
    /// transcript records whatever the agent passed; resolution is a caller's
    /// job, exactly as with [`super::FileTouch`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub path: Option<String>,
    /// `(start, end)` 1-based inclusive line range — ONLY when the `Read`
    /// call genuinely carried `offset`/`limit`. Measured on a real
    /// transcript, 104 of 131 Read calls carried `file_path` alone, so this
    /// is `None` far more often than not; it is never inferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub line_range: Option<(u32, u32)>,
    /// The edited text region for an [`Edit`](ReplayKind::Edit) beat (an
    /// `Edit` carries `old_string`/`new_string`, never a line range), from
    /// `new_string` when present else `old_string`, truncated to
    /// [`REPLAY_SNIPPET_MAX_CHARS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub snippet: Option<String>,
    /// How many source beats folded into this one. `1` for a lone beat.
    pub count: u32,
    /// R7/S6 — the `t-<uuid12>` id of the `session-view/1` turn this beat's
    /// source JSONL record belongs to (the record's own `uuid`, transformed
    /// identically to `sessions::view::turn_id_from_uuid`) — the two-way
    /// transcript↔replay handoff key (`?turn=`). `None` for a record with no
    /// `uuid` (very old transcripts). Best-effort: a record the view engine
    /// folds into an EARLIER turn's requestId-merge group still stamps its
    /// OWN uuid here, which may not be that merged turn's seed id — the
    /// handoff degrades to "nearest turn", never a hard error (S6 — replay
    /// deliberately doesn't rebuild the full merge pass just for this ref).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub turn: Option<String>,
}

impl ReplayBeat {
    /// The collapse key: two consecutive beats fold only when this matches
    /// (and they are within [`ReplayOptions::collapse_window_secs`]). The
    /// FIRST beat of a run keeps its `line_range` / `snippet`.
    fn run_key(&self) -> (ReplayKind, Option<&str>, &str) {
        (self.kind, self.path.as_deref(), self.detail.as_str())
    }
}

/// Knobs for [`replay_timeline`]. `Default` is the shipped behaviour; the
/// struct exists so a CLI can widen the cap for an export without the SPA
/// route inheriting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOptions {
    /// See [`REPLAY_MAX_EVENTS`]. `0` means "no beats at all" (everything is
    /// dropped and reported), not "unlimited".
    pub max_events: usize,
    /// See [`REPLAY_COLLAPSE_WINDOW_SECS`]. `0` disables collapsing.
    pub collapse_window_secs: i64,
    /// Emit [`ReplayKind::Assistant`] beats for the agent's prose. Off gives
    /// a pure "what the agent DID" timeline (tools + prompts + decisions).
    pub include_assistant: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            max_events: REPLAY_MAX_EVENTS,
            collapse_window_secs: REPLAY_COLLAPSE_WINDOW_SECS,
            include_assistant: true,
        }
    }
}

/// The full result of a replay extraction. Every counter is a *reported*
/// number: nothing is dropped without appearing here.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayTimeline {
    /// Always [`REPLAY_GRAMMAR`] — on the wire so a consumer can refuse a
    /// shape it doesn't know.
    pub grammar: String,
    pub beats: Vec<ReplayBeat>,
    /// First / last beat instant (`None` on an empty timeline).
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// `ended_at - started_at`, `0` when either is missing. Derived from the
    /// BEATS, so it is never larger than the session (a metadata record with
    /// a later timestamp doesn't extend a replay).
    pub duration_secs: i64,
    /// Non-empty JSONL lines seen (parse failures included — the transcript
    /// is ground truth even when one line is corrupt).
    pub records: usize,
    /// Records carrying no parseable ISO `timestamp` — metadata only
    /// (last-prompt / mode / permission-mode / ai-title /
    /// file-history-snapshot). Skipped, not silently.
    pub metadata_skipped: usize,
    /// How many beats had a timestamp earlier than the beat before them.
    pub out_of_order: usize,
    /// How many beats folded into a run (so
    /// `beats.len() + collapsed + dropped` == beats that would have existed).
    pub collapsed: usize,
    /// True when [`ReplayOptions::max_events`] bit.
    pub truncated: bool,
    /// Beats dropped by the cap.
    pub dropped: usize,
}

impl ReplayTimeline {
    fn empty() -> Self {
        Self {
            grammar: REPLAY_GRAMMAR.to_string(),
            beats: Vec::new(),
            started_at: None,
            ended_at: None,
            duration_secs: 0,
            records: 0,
            metadata_skipped: 0,
            out_of_order: 0,
            collapsed: 0,
            truncated: false,
            dropped: 0,
        }
    }
}

/// Pure: turn a recovered JSONL transcript into a [`ReplayTimeline`].
///
/// ```
/// use kb_core::sessions::replay::{replay_timeline, ReplayKind, ReplayOptions};
///
/// let jsonl = concat!(
///     r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"user","content":"fix the parser"}}"#,
///     "\n",
///     r#"{"timestamp":"2026-07-20T10:00:07Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/p/src/parser.rs"}}]}}"#,
/// );
/// let tl = replay_timeline(jsonl, &ReplayOptions::default());
/// assert_eq!(tl.beats.len(), 2);
/// assert_eq!(tl.beats[1].kind, ReplayKind::Read);
/// assert_eq!(tl.beats[1].delta_secs, 7);
/// ```
pub fn replay_timeline(jsonl: &str, opts: &ReplayOptions) -> ReplayTimeline {
    let mut tl = ReplayTimeline::empty();
    let mut clock = Clock::default();
    // R3/R7 — the session's closure text (same selection rule
    // `closing_assistant_text` and `session-view/1`'s outcome both use),
    // computed once so the Outcome beat kind can be assigned in this SAME
    // pass rather than re-scanning the transcript a second time.
    let closing_text = super::closing_assistant_text(jsonl);

    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        tl.records += 1;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            // Unparseable line: it has no recoverable timestamp, so it is
            // metadata as far as the replay is concerned.
            tl.metadata_skipped += 1;
            continue;
        };
        let Some(ts) = v
            .get("timestamp")
            .and_then(|x| x.as_str())
            .and_then(parse_iso_utc)
        else {
            tl.metadata_skipped += 1;
            continue;
        };

        let role = v
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(|x| x.as_str());

        // R7/S6 — every beat this record produces shares the record's own
        // turn ref (see `ReplayBeat::turn`'s doc for the approximation this
        // makes on a requestId-merged record).
        let turn_ref = v
            .get("uuid")
            .and_then(|x| x.as_str())
            .map(super::view::turn_id_from_uuid);

        let mut pending: Vec<ReplayBeat> = Vec::new();
        match role {
            Some("user") => user_beats(&v, ts, &mut pending),
            Some("assistant") => {
                assistant_beats(&v, ts, opts, closing_text.as_deref(), &mut pending)
            }
            _ => {}
        }

        for mut beat in pending {
            beat.turn = turn_ref.clone();
            push_beat(&mut tl, beat, opts, &mut clock);
        }
    }

    tl.started_at = tl.beats.first().map(|b| b.ts_unix);
    tl.ended_at = tl.beats.last().map(|b| b.ts_unix);
    if let (Some(a), Some(b)) = (tl.started_at, tl.ended_at) {
        tl.duration_secs = (b - a).max(0);
    }
    tl
}

/// The two instants the append pass carries between beats. Kept OUT of
/// [`ReplayTimeline`] (which is a wire type) so the serialized shape stays
/// exactly the contract.
#[derive(Debug, Default)]
struct Clock {
    /// Instant the NEXT beat's `delta_secs` is measured from: the FIRST
    /// instant of the tail run, so folding a run can't inflate the gap that
    /// follows it.
    prev_ts: Option<i64>,
    /// Instant of the LAST beat folded into the tail run — the rolling
    /// window anchor, so a 400-read run at 1 s/read folds whole.
    collapse_anchor: i64,
}

/// Append one beat, folding it into the tail run when eligible and honouring
/// the cap.
fn push_beat(
    tl: &mut ReplayTimeline,
    mut beat: ReplayBeat,
    opts: &ReplayOptions,
    clock: &mut Clock,
) {
    // Run-collapsing: same kind + same path + same label, within the window
    // of the PREVIOUS beat in the run. Only collapsible kinds fold.
    if opts.collapse_window_secs > 0 && beat.kind.collapsible() {
        let anchor = clock.collapse_anchor;
        if let Some(last) = tl.beats.last_mut() {
            if last.run_key() == beat.run_key()
                && beat.ts_unix >= anchor
                && beat.ts_unix - anchor <= opts.collapse_window_secs
            {
                last.count = last.count.saturating_add(1);
                tl.collapsed += 1;
                clock.collapse_anchor = beat.ts_unix;
                return;
            }
        }
    }

    if tl.beats.len() >= opts.max_events {
        tl.truncated = true;
        tl.dropped += 1;
        return;
    }

    beat.delta_secs = match clock.prev_ts {
        None => 0,
        Some(p) => {
            if beat.ts_unix < p {
                tl.out_of_order += 1;
                0
            } else {
                beat.ts_unix - p
            }
        }
    };
    beat.seq = tl.beats.len();
    clock.prev_ts = Some(beat.ts_unix);
    clock.collapse_anchor = beat.ts_unix;
    tl.beats.push(beat);
}

/// A bare beat with the derived fields (`seq` / `delta_secs` / `turn`) left
/// to [`push_beat`] / [`replay_timeline`].
fn beat(kind: ReplayKind, ts: i64, detail: String) -> ReplayBeat {
    ReplayBeat {
        seq: 0,
        ts_unix: ts,
        delta_secs: 0,
        kind,
        detail: truncate_chars(detail.trim(), REPLAY_DETAIL_MAX_CHARS),
        path: None,
        line_range: None,
        snippet: None,
        count: 1,
        turn: None,
    }
}

/// Beats from one `role:"user"` record: the human's typed turn and any
/// steering decision it carried. A tool_result-only user turn (the usual
/// case) produces nothing — the *call* already got a beat on the assistant
/// record, where the clock for "when it happened" belongs.
fn user_beats(v: &serde_json::Value, ts: i64, out: &mut Vec<ReplayBeat>) {
    let is_meta = v.get("isMeta").and_then(|x| x.as_bool()).unwrap_or(false);
    let is_sidechain = v
        .get("isSidechain")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    if !is_meta && !is_sidechain {
        if let Some(text) = user_message_text(v) {
            if !is_wrapper_text(&text) {
                out.push(beat(ReplayKind::Prompt, ts, first_line(&text)));
            }
        }
    }

    // Steering decisions ride the same extractor the decisions log uses, so
    // the replay and `session_decisions` can never disagree about what
    // counts as a decision.
    let mut decisions: Vec<Decision> = Vec::new();
    let mut errors: u32 = 0;
    super::extract_decisions_and_errors(v, &mut decisions, &mut errors);
    for d in decisions {
        let label = match d.answer {
            Some(a) => format!("{} -> {}", d.prompt.trim(), a.trim()),
            None => d.prompt.trim().to_string(),
        };
        out.push(beat(ReplayKind::Decision, ts, label));
    }
}

/// Beats from one `role:"assistant"` record: the prose it narrated (when
/// enabled) followed by its tool calls, in block order.
fn assistant_beats(
    v: &serde_json::Value,
    ts: i64,
    opts: &ReplayOptions,
    closing_text: Option<&str>,
    out: &mut Vec<ReplayBeat>,
) {
    let Some(blocks) = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
    else {
        return;
    };

    if opts.include_assistant {
        let text: String = blocks
            .iter()
            .filter(|b| b.get("type").and_then(|x| x.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|x| x.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            // R3/R7 — the record whose FULL prose matches the transcript's
            // closure (`closing_assistant_text`'s selection, same eligibility
            // rule) gets the Outcome kind instead of a plain Assistant beat.
            let kind = if closing_text == Some(trimmed) {
                ReplayKind::Outcome
            } else {
                ReplayKind::Assistant
            };
            out.push(beat(kind, ts, first_line(trimmed)));
        }
    }

    for b in blocks {
        if b.get("type").and_then(|x| x.as_str()) != Some("tool_use") {
            continue;
        }
        let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("");
        let input = b.get("input");
        tool_beats(name, input, ts, out);
    }
}

/// One `tool_use` block → zero or more beats.
fn tool_beats(name: &str, input: Option<&serde_json::Value>, ts: i64, out: &mut Vec<ReplayBeat>) {
    let field = |k: &str| -> Option<String> {
        input
            .and_then(|i| i.get(k))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    let path = || -> Option<String> { field("file_path").or_else(|| field("notebook_path")) };

    match name {
        "Read" => {
            let p = path();
            let mut b = beat(ReplayKind::Read, ts, label_for_path(p.as_deref(), name));
            b.line_range = read_line_range(input);
            b.path = p;
            out.push(b);
        }
        "Write" => {
            let p = path();
            let mut b = beat(ReplayKind::Write, ts, label_for_path(p.as_deref(), name));
            b.path = p;
            out.push(b);
        }
        "Edit" | "MultiEdit" | "NotebookEdit" => {
            let p = path();
            let mut b = beat(ReplayKind::Edit, ts, label_for_path(p.as_deref(), name));
            b.snippet = edit_snippet(input);
            b.path = p;
            out.push(b);
        }
        "Bash" => {
            let cmd = field("command").unwrap_or_default();
            // A VCS action is a commit beat (one per chained segment), not a
            // bash beat — `git commit && git push` is two moments.
            let actions = git_action_of(&cmd);
            if actions.is_empty() {
                out.push(beat(ReplayKind::Bash, ts, first_line(&cmd)));
            } else {
                for (kind, subject) in actions {
                    let label = match subject {
                        Some(s) if s != kind => format!("{kind}: {s}"),
                        _ => kind,
                    };
                    out.push(beat(ReplayKind::Commit, ts, label));
                }
            }
        }
        "Grep" | "Glob" => {
            let q = field("pattern")
                .or_else(|| field("query"))
                .unwrap_or_default();
            out.push(beat(ReplayKind::Search, ts, format!("{name}: {q}")));
        }
        "WebSearch" => {
            let q = field("query").unwrap_or_default();
            out.push(beat(ReplayKind::Search, ts, format!("web: {q}")));
        }
        "WebFetch" => {
            let q = field("url")
                .or_else(|| field("prompt"))
                .or_else(|| field("query"))
                .unwrap_or_default();
            out.push(beat(ReplayKind::Search, ts, format!("web: {q}")));
        }
        // The subagent-delegation tool: "Task" historically, "Agent" in newer
        // transcripts (both are in the wild — mirror `classify_research`).
        "Task" | "Agent" => {
            let q = field("description")
                .or_else(|| field("subagent_type"))
                .or_else(|| field("prompt"))
                .unwrap_or_default();
            out.push(beat(ReplayKind::Subagent, ts, first_line(&q)));
        }
        "" => {}
        other => out.push(beat(ReplayKind::Other, ts, other.to_string())),
    }
}

/// `(start, end)` 1-based inclusive, ONLY when the call genuinely carried
/// `offset` and/or `limit`. `offset` alone → a single-line marker at the
/// offset; `limit` alone → from line 1. Never inferred when both are absent
/// (the common case).
fn read_line_range(input: Option<&serde_json::Value>) -> Option<(u32, u32)> {
    let num = |k: &str| -> Option<u32> {
        input
            .and_then(|i| i.get(k))
            .and_then(|x| x.as_u64())
            .map(|n| n.min(u32::MAX as u64) as u32)
    };
    let offset = num("offset");
    let limit = num("limit");
    if offset.is_none() && limit.is_none() {
        return None;
    }
    let start = offset.unwrap_or(1).max(1);
    let end = match limit {
        Some(l) if l > 0 => start.saturating_add(l - 1),
        _ => start,
    };
    Some((start, end))
}

/// The edited text region from an Edit/MultiEdit/NotebookEdit input:
/// `new_string` when present, else `old_string`, else the first entry of a
/// MultiEdit `edits` array. Truncated to [`REPLAY_SNIPPET_MAX_CHARS`].
fn edit_snippet(input: Option<&serde_json::Value>) -> Option<String> {
    let pick = |v: &serde_json::Value| -> Option<String> {
        v.get("new_string")
            .or_else(|| v.get("old_string"))
            .or_else(|| v.get("new_source"))
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| truncate_chars(t, REPLAY_SNIPPET_MAX_CHARS))
    };
    let input = input?;
    pick(input).or_else(|| {
        input
            .get("edits")
            .and_then(|e| e.as_array())
            .and_then(|a| a.first())
            .and_then(pick)
    })
}

/// Basename label for a file beat; falls back to the tool name when the call
/// carried no path at all.
fn label_for_path(path: Option<&str>, tool: &str) -> String {
    match path {
        Some(p) => p
            .trim_end_matches('/')
            .rsplit(['/', '\\'])
            .next()
            .filter(|b| !b.is_empty())
            .unwrap_or(p)
            .to_string(),
        None => tool.to_string(),
    }
}

/// First non-empty line, whitespace-trimmed — a one-line label for prose.
fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-built transcript exercising every kind. This fixture + the
    /// golden assertion below ARE the `session-replay/1` contract the CLI and
    /// the SPA share — change them only with the grammar tag.
    const GOLDEN_JSONL: &str = concat!(
        // metadata: no timestamp at all.
        r#"{"type":"file-history-snapshot","snapshot":{"trackedFileBackups":{}}}"#,
        "\n",
        r#"{"timestamp":"2026-07-20T10:00:00Z","promptSource":"typed","message":{"role":"user","content":"fix the flaky parser\nplease"}}"#,
        "\n",
        r#"{"timestamp":"2026-07-20T10:00:05Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"Let me look at the parser."},{"type":"tool_use","name":"Read","input":{"file_path":"/p/src/parser.rs"}}]}}"#,
        "\n",
        // Two more reads of the same file inside the window → collapse to count=3.
        r#"{"timestamp":"2026-07-20T10:00:09Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/p/src/parser.rs"}},{"type":"tool_use","name":"Read","input":{"file_path":"/p/src/parser.rs"}}]}}"#,
        "\n",
        // A Read WITH offset/limit → its own beat (different key? same key —
        // but it lands after a Grep, so the run is broken anyway).
        r#"{"timestamp":"2026-07-20T10:00:20Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Grep","input":{"pattern":"fn parse"}},{"type":"tool_use","name":"Read","input":{"file_path":"/p/src/parser.rs","offset":40,"limit":20}}]}}"#,
        "\n",
        r#"{"timestamp":"2026-07-20T10:01:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/p/src/parser.rs","old_string":"a","new_string":"let x = 1;"}},{"type":"tool_use","name":"Task","input":{"description":"review the fix"}}]}}"#,
        "\n",
        // AskUserQuestion answer → a decision beat.
        r#"{"timestamp":"2026-07-20T10:02:00Z","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]},"toolUseResult":{"questions":[{"question":"Which fix approach?"}],"answers":{"Which fix approach?":"root cause"}}}"#,
        "\n",
        // Bash: a plain command, then a chained commit && push.
        r#"{"timestamp":"2026-07-20T10:03:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test -p kb-core"}},{"type":"tool_use","name":"Bash","input":{"command":"git commit -m \"fix(parser): handle empty pre\" && git push"}}]}}"#,
        "\n",
        // A synthetic wrapper user turn → NOT a prompt beat.
        r#"{"timestamp":"2026-07-20T10:03:30Z","message":{"role":"user","content":"<system-reminder>be good</system-reminder>"}}"#,
        "\n",
        // An unknown tool → `other`.
        r#"{"timestamp":"2026-07-20T10:04:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"TodoWrite","input":{"todos":[]}}]}}"#,
        "\n",
    );

    fn compact(tl: &ReplayTimeline) -> Vec<(usize, i64, i64, &'static str, String, u32)> {
        tl.beats
            .iter()
            .map(|b| {
                (
                    b.seq,
                    b.ts_unix,
                    b.delta_secs,
                    b.kind.as_str(),
                    b.detail.clone(),
                    b.count,
                )
            })
            .collect()
    }

    // The golden pin — the shape the CLI and SPA both consume.
    #[test]
    fn replay_golden_timeline_is_pinned() {
        let tl = replay_timeline(GOLDEN_JSONL, &ReplayOptions::default());
        let base = 1_784_541_600; // 2026-07-20T10:00:00Z
        assert_eq!(parse_iso_utc("2026-07-20T10:00:00Z"), Some(base));
        let want: Vec<(usize, i64, i64, &str, String, u32)> = vec![
            (0, base, 0, "prompt", "fix the flaky parser".into(), 1),
            // R3/R7 — this is the ONLY eligible assistant prose in the whole
            // golden transcript, so it IS the transcript's closure: the
            // Outcome kind, not a plain Assistant beat.
            (
                1,
                base + 5,
                5,
                "outcome",
                "Let me look at the parser.".into(),
                1,
            ),
            // 3 reads of one file inside the window → ONE beat, count 3.
            (2, base + 5, 0, "read", "parser.rs".into(), 3),
            (3, base + 20, 15, "search", "Grep: fn parse".into(), 1),
            (4, base + 20, 0, "read", "parser.rs".into(), 1),
            (5, base + 60, 40, "edit", "parser.rs".into(), 1),
            (6, base + 60, 0, "subagent", "review the fix".into(), 1),
            (
                7,
                base + 120,
                60,
                "decision",
                "Which fix approach? -> root cause".into(),
                1,
            ),
            (8, base + 180, 60, "bash", "cargo test -p kb-core".into(), 1),
            (
                9,
                base + 180,
                0,
                "commit",
                "commit: fix(parser): handle empty pre".into(),
                1,
            ),
            (10, base + 180, 0, "commit", "push".into(), 1),
            (11, base + 240, 60, "other", "TodoWrite".into(), 1),
        ];
        assert_eq!(compact(&tl), want, "{tl:#?}");
        assert_eq!(tl.grammar, "session-replay/1");
        assert_eq!(tl.records, 10);
        assert_eq!(tl.metadata_skipped, 1);
        assert_eq!(tl.collapsed, 2);
        assert_eq!(tl.out_of_order, 0);
        assert!(!tl.truncated);
        assert_eq!(tl.dropped, 0);
        assert_eq!(tl.started_at, Some(base));
        assert_eq!(tl.ended_at, Some(base + 240));
        assert_eq!(tl.duration_secs, 240);
    }

    #[test]
    fn replay_golden_carries_path_line_range_and_snippet() {
        let tl = replay_timeline(GOLDEN_JSONL, &ReplayOptions::default());
        // The collapsed run keeps the FIRST beat's (absent) range.
        assert_eq!(tl.beats[2].path.as_deref(), Some("/p/src/parser.rs"));
        assert_eq!(tl.beats[2].line_range, None);
        // The offset/limit read genuinely carried one.
        assert_eq!(tl.beats[4].line_range, Some((40, 59)));
        // The edit carries the new_string snippet, no line range.
        assert_eq!(tl.beats[5].snippet.as_deref(), Some("let x = 1;"));
        assert_eq!(tl.beats[5].line_range, None);
        // Prompt/assistant beats never carry a path.
        assert!(tl.beats[0].path.is_none() && tl.beats[1].path.is_none());
    }

    #[test]
    fn replay_json_round_trips() {
        let tl = replay_timeline(GOLDEN_JSONL, &ReplayOptions::default());
        let json = serde_json::to_string(&tl).expect("serialize");
        let back: ReplayTimeline = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tl, back);
        assert!(json.contains(r#""kind":"read""#), "{json}");
    }

    #[test]
    fn kind_wire_tokens_round_trip() {
        for k in [
            ReplayKind::Prompt,
            ReplayKind::Assistant,
            ReplayKind::Read,
            ReplayKind::Edit,
            ReplayKind::Write,
            ReplayKind::Bash,
            ReplayKind::Commit,
            ReplayKind::Decision,
            ReplayKind::Search,
            ReplayKind::Subagent,
            ReplayKind::Outcome,
            ReplayKind::Other,
        ] {
            assert_eq!(ReplayKind::from_wire(k.as_str()), Some(k));
        }
        assert_eq!(ReplayKind::from_wire("nope"), None);
    }

    #[test]
    fn replay_empty_input_is_an_empty_timeline() {
        for src in ["", "\n", "   \n\n  "] {
            let tl = replay_timeline(src, &ReplayOptions::default());
            assert!(tl.beats.is_empty());
            assert_eq!(tl.records, 0);
            assert_eq!(tl.metadata_skipped, 0);
            assert_eq!(tl.started_at, None);
            assert_eq!(tl.ended_at, None);
            assert_eq!(tl.duration_secs, 0);
            assert!(!tl.truncated);
        }
    }

    #[test]
    fn replay_single_record_has_zero_delta() {
        let src =
            r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"user","content":"hello"}}"#;
        let tl = replay_timeline(src, &ReplayOptions::default());
        assert_eq!(tl.beats.len(), 1);
        assert_eq!(tl.beats[0].delta_secs, 0);
        assert_eq!(tl.beats[0].seq, 0);
        assert_eq!(tl.beats[0].count, 1);
        assert_eq!(tl.duration_secs, 0);
    }

    #[test]
    fn replay_all_metadata_input_yields_no_beats_but_counts() {
        let src = concat!(
            r#"{"type":"file-history-snapshot","snapshot":{}}"#,
            "\n",
            r#"{"aiTitle":"Qwortzle investigation"}"#,
            "\n",
            r#"{"type":"summary","summary":"whatever"}"#,
            "\n",
            "not json at all",
            "\n",
        );
        let tl = replay_timeline(src, &ReplayOptions::default());
        assert!(tl.beats.is_empty());
        assert_eq!(tl.records, 4);
        assert_eq!(tl.metadata_skipped, 4);
        assert!(!tl.truncated);
        assert_eq!(tl.dropped, 0);
    }

    #[test]
    fn replay_timestamped_record_with_nothing_to_show_is_not_metadata() {
        // A tool_result-only user turn HAS a timestamp (so it isn't
        // metadata) but produces no beat — the call already got one.
        let src = r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#;
        let tl = replay_timeline(src, &ReplayOptions::default());
        assert!(tl.beats.is_empty());
        assert_eq!(tl.records, 1);
        assert_eq!(tl.metadata_skipped, 0);
    }

    // Ruling: transcript order IS causal order, so we never sort. An
    // inverted clock keeps its raw ts_unix and clamps only the delta.
    #[test]
    fn replay_preserves_transcript_order_and_clamps_backwards_clock() {
        let src = concat!(
            r#"{"timestamp":"2026-07-20T10:00:30Z","message":{"role":"user","content":"second in time"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"user","content":"earlier stamp, later in file"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:01:00Z","message":{"role":"user","content":"third"}}"#,
            "\n",
        );
        let tl = replay_timeline(src, &ReplayOptions::default());
        let details: Vec<_> = tl.beats.iter().map(|b| b.detail.as_str()).collect();
        assert_eq!(
            details,
            vec!["second in time", "earlier stamp, later in file", "third"]
        );
        // Raw instants preserved…
        assert_eq!(tl.beats[1].ts_unix, tl.beats[0].ts_unix - 30);
        // …but the playhead clock never runs backwards.
        assert_eq!(tl.beats[1].delta_secs, 0);
        assert_eq!(tl.beats[2].delta_secs, 60);
        assert_eq!(tl.out_of_order, 1);
        // started/ended follow the BEATS, not min/max of the stamps.
        assert_eq!(tl.started_at, Some(tl.beats[0].ts_unix));
        assert_eq!(tl.ended_at, Some(tl.beats[2].ts_unix));
    }

    #[test]
    fn replay_collapses_a_long_run_of_identical_reads() {
        let mut src = String::new();
        for i in 0..400 {
            // One read per second — all inside the rolling window.
            let ts = format!("2026-07-20T10:{:02}:{:02}Z", i / 60, i % 60);
            src.push_str(&format!(
                r#"{{"timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"/p/a.rs"}}}}]}}}}"#
            ));
            src.push('\n');
        }
        let tl = replay_timeline(&src, &ReplayOptions::default());
        assert_eq!(tl.beats.len(), 1, "400 reads of one file → one beat");
        assert_eq!(tl.beats[0].count, 400);
        assert_eq!(tl.collapsed, 399);
        assert!(!tl.truncated);
    }

    #[test]
    fn replay_run_breaks_on_window_path_and_kind() {
        let read = |ts: &str, p: &str| {
            format!(
                r#"{{"timestamp":"{ts}","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"{p}"}}}}]}}}}"#
            )
        };
        let src = [
            read("2026-07-20T10:00:00Z", "/p/a.rs"),
            read("2026-07-20T10:00:10Z", "/p/a.rs"),
            // different path → new beat
            read("2026-07-20T10:00:20Z", "/p/b.rs"),
            // back to a.rs → new beat (run was broken)
            read("2026-07-20T10:00:30Z", "/p/a.rs"),
            // > window after the previous a.rs read → new beat
            read("2026-07-20T10:10:00Z", "/p/a.rs"),
        ]
        .join("\n");
        let tl = replay_timeline(&src, &ReplayOptions::default());
        let shape: Vec<_> = tl
            .beats
            .iter()
            .map(|b| (b.detail.as_str(), b.count))
            .collect();
        assert_eq!(
            shape,
            vec![("a.rs", 2), ("b.rs", 1), ("a.rs", 1), ("a.rs", 1)]
        );
        assert_eq!(tl.collapsed, 1);
    }

    #[test]
    fn replay_narrative_kinds_never_collapse() {
        let src = concat!(
            r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"user","content":"same"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:00:01Z","message":{"role":"user","content":"same"}}"#,
            "\n",
        );
        let tl = replay_timeline(src, &ReplayOptions::default());
        assert_eq!(tl.beats.len(), 2);
        assert_eq!(tl.collapsed, 0);
    }

    #[test]
    fn replay_collapse_window_zero_disables_folding() {
        let src = concat!(
            r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/p/a.rs"}}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:00:01Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"/p/a.rs"}}]}}"#,
            "\n",
        );
        let opts = ReplayOptions {
            collapse_window_secs: 0,
            ..Default::default()
        };
        let tl = replay_timeline(src, &opts);
        assert_eq!(tl.beats.len(), 2);
        assert_eq!(tl.collapsed, 0);
    }

    #[test]
    fn replay_cap_truncates_honestly() {
        let mut src = String::new();
        for i in 0..10 {
            src.push_str(&format!(
                r#"{{"timestamp":"2026-07-20T10:00:{i:02}Z","message":{{"role":"user","content":"turn {i}"}}}}"#
            ));
            src.push('\n');
        }
        let opts = ReplayOptions {
            max_events: 4,
            ..Default::default()
        };
        let tl = replay_timeline(&src, &opts);
        assert_eq!(tl.beats.len(), 4);
        assert!(tl.truncated);
        assert_eq!(tl.dropped, 6);
        // The kept prefix is the FIRST beats, in order.
        assert_eq!(tl.beats[3].detail, "turn 3");
        // Counters still add up to the beats that would have existed.
        assert_eq!(tl.beats.len() + tl.collapsed + tl.dropped, 10);
    }

    #[test]
    fn replay_cap_zero_drops_everything_but_reports_it() {
        let src =
            r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"user","content":"hi"}}"#;
        let opts = ReplayOptions {
            max_events: 0,
            ..Default::default()
        };
        let tl = replay_timeline(src, &opts);
        assert!(tl.beats.is_empty());
        assert!(tl.truncated);
        assert_eq!(tl.dropped, 1);
    }

    #[test]
    fn replay_collapsing_past_the_cap_still_counts() {
        // 3 reads: the first fills a cap of 1, the other two fold into it.
        let mut src = String::new();
        for i in 0..3 {
            src.push_str(&format!(
                r#"{{"timestamp":"2026-07-20T10:00:{i:02}Z","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Read","input":{{"file_path":"/p/a.rs"}}}}]}}}}"#
            ));
            src.push('\n');
        }
        let opts = ReplayOptions {
            max_events: 1,
            ..Default::default()
        };
        let tl = replay_timeline(&src, &opts);
        assert_eq!(tl.beats.len(), 1);
        assert_eq!(tl.beats[0].count, 3);
        assert!(!tl.truncated, "folding into the tail never truncates");
        assert_eq!(tl.dropped, 0);
    }

    #[test]
    fn replay_include_assistant_false_drops_prose_only() {
        let opts = ReplayOptions {
            include_assistant: false,
            ..Default::default()
        };
        let tl = replay_timeline(GOLDEN_JSONL, &opts);
        assert!(tl.beats.iter().all(|b| b.kind != ReplayKind::Assistant));
        // R3/R7 — the Outcome kind rides the SAME include_assistant gate: no
        // prose emission at all means no Outcome beat either.
        assert!(tl.beats.iter().all(|b| b.kind != ReplayKind::Outcome));
        assert!(tl.beats.iter().any(|b| b.kind == ReplayKind::Read));
        // Seqs stay dense after the drop.
        for (i, b) in tl.beats.iter().enumerate() {
            assert_eq!(b.seq, i);
        }
    }

    #[test]
    fn replay_read_line_range_only_when_the_call_carried_one() {
        let mk = |input: &str| {
            format!(
                r#"{{"timestamp":"2026-07-20T10:00:00Z","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Read","input":{input}}}]}}}}"#
            )
        };
        let range = |input: &str| {
            replay_timeline(&mk(input), &ReplayOptions::default()).beats[0].line_range
        };
        assert_eq!(range(r#"{"file_path":"/p/a.rs"}"#), None);
        assert_eq!(
            range(r#"{"file_path":"/p/a.rs","offset":40,"limit":20}"#),
            Some((40, 59))
        );
        assert_eq!(
            range(r#"{"file_path":"/p/a.rs","offset":40}"#),
            Some((40, 40))
        );
        assert_eq!(
            range(r#"{"file_path":"/p/a.rs","limit":30}"#),
            Some((1, 30))
        );
        // A garbage offset can't panic or wrap.
        assert_eq!(
            range(r#"{"file_path":"/p/a.rs","offset":0,"limit":0}"#),
            Some((1, 1))
        );
    }

    #[test]
    fn replay_edit_snippet_prefers_new_string_and_truncates() {
        let long = "x".repeat(REPLAY_SNIPPET_MAX_CHARS + 50);
        let src = format!(
            r#"{{"timestamp":"2026-07-20T10:00:00Z","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Edit","input":{{"file_path":"/p/a.rs","old_string":"old","new_string":"{long}"}}}}]}}}}"#
        );
        let tl = replay_timeline(&src, &ReplayOptions::default());
        let snip = tl.beats[0].snippet.as_deref().unwrap();
        assert_eq!(snip.chars().count(), REPLAY_SNIPPET_MAX_CHARS);

        // old_string alone is used when there's no new_string (a deletion).
        let src = r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/p/a.rs","old_string":"gone"}}]}}"#;
        let tl = replay_timeline(src, &ReplayOptions::default());
        assert_eq!(tl.beats[0].snippet.as_deref(), Some("gone"));

        // MultiEdit takes the first edit in the array.
        let src = r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"MultiEdit","input":{"file_path":"/p/a.rs","edits":[{"old_string":"a","new_string":"first"},{"old_string":"b","new_string":"second"}]}}]}}"#;
        let tl = replay_timeline(src, &ReplayOptions::default());
        assert_eq!(tl.beats[0].kind, ReplayKind::Edit);
        assert_eq!(tl.beats[0].snippet.as_deref(), Some("first"));
    }

    #[test]
    fn replay_detail_is_truncated_and_single_line() {
        let long = "y".repeat(REPLAY_DETAIL_MAX_CHARS + 40);
        let src = format!(
            r#"{{"timestamp":"2026-07-20T10:00:00Z","message":{{"role":"user","content":"{long}\nsecond line"}}}}"#
        );
        let tl = replay_timeline(&src, &ReplayOptions::default());
        assert_eq!(tl.beats[0].detail.chars().count(), REPLAY_DETAIL_MAX_CHARS);
        assert!(!tl.beats[0].detail.contains('\n'));
    }

    #[test]
    fn replay_skips_sidechain_and_meta_user_turns() {
        let src = concat!(
            r#"{"timestamp":"2026-07-20T10:00:00Z","isMeta":true,"message":{"role":"user","content":"meta"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:00:01Z","isSidechain":true,"message":{"role":"user","content":"sidechain"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:00:02Z","message":{"role":"user","content":"real"}}"#,
            "\n",
        );
        let tl = replay_timeline(src, &ReplayOptions::default());
        assert_eq!(tl.beats.len(), 1);
        assert_eq!(tl.beats[0].detail, "real");
    }

    #[test]
    fn replay_is_deterministic_and_clock_free() {
        let a = replay_timeline(GOLDEN_JSONL, &ReplayOptions::default());
        let b = replay_timeline(GOLDEN_JSONL, &ReplayOptions::default());
        assert_eq!(a, b);
    }

    #[test]
    fn replay_matches_parse_session_activity_end_stamp() {
        // The playhead clock and `ended_at` share one parser, so the last
        // beat can never disagree with the session row.
        let act = super::super::parse_session_activity(GOLDEN_JSONL);
        let tl = replay_timeline(GOLDEN_JSONL, &ReplayOptions::default());
        assert_eq!(act.ended_at, tl.ended_at);
    }

    #[test]
    fn replay_tolerates_malformed_and_hostile_input() {
        let src = concat!(
            "{not json\n",
            r#"{"timestamp":"","message":{"role":"user","content":"empty ts"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:00:00Z","message":{"role":"assistant","content":"a bare string, not blocks"}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:00:01Z","message":{"role":"assistant","content":[{"type":"tool_use"}]}}"#,
            "\n",
            r#"{"timestamp":"2026-07-20T10:00:02Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{}}]}}"#,
            "\n",
        );
        let tl = replay_timeline(src, &ReplayOptions::default());
        // The nameless tool_use is ignored; the path-less Read still beats.
        assert_eq!(tl.beats.len(), 1);
        assert_eq!(tl.beats[0].kind, ReplayKind::Read);
        assert_eq!(tl.beats[0].detail, "Read");
        assert_eq!(tl.beats[0].path, None);
        assert_eq!(tl.metadata_skipped, 2);
    }
}
