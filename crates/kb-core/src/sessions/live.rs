//! LSC-1 — the live-sessions cockpit's two-axis state model
//! (`docs/research/kb-live-sessions-cockpit-2026-08.html` §3 "The model:
//! two axes, not three buckets") plus the Claude Code transcript adapter
//! (§4 "Collection, harness by harness" → "Claude Code — the dominant
//! case", §12 "Evidence appendix").
//!
//! The whole discipline in one sentence: **who holds the ball** (agent /
//! human / ended) and **how long since it moved** (seconds + a confidence
//! label) are INDEPENDENT axes. Silence never flips the holder axis — a
//! twenty-minute `cargo build` appends nothing to the transcript, so a
//! recency-only rule would misread a working session as "waiting on you".
//! [`derive_state`] is the one pure function that turns the two axes into
//! the six lanes the design names; [`classify_claude_transcript`] is the
//! ONLY adapter this phase ships (Claude Code is ~90% of the value per the
//! design's measured evidence: 55 main transcripts / 191 MB vs. 4 Codex
//! rollouts in the same 14-day window).
//!
//! This module is pull-only / direct-disk (design phase L1): no daemon, no
//! HTTP, no storage. It reuses [`super::tail`]'s byte-offset discipline in
//! spirit (bounded, never-full-parse reads) but not its exact bootstrap
//! window — a live-status classifier wants "at most the last
//! [`super::constants::LIVE_TAIL_BYTES`], first partial line discarded",
//! which is the validated Python spike's `tail_records` shape (§12), not
//! `tail::read_bootstrap_window`'s additional trailing-line trim (that
//! helper feeds `session_view`, which needs a clean line-bounded window to
//! join against; this module tolerates — and silently skips — a torn
//! trailing line instead, since JSON parse failure already IS the "skip
//! it" signal here).

use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::constants::{ABANDON_AFTER_SECS, COLD_AFTER_SECS, LIVE_TAIL_BYTES, STALL_AFTER_SECS};
use super::HARNESS_DEFAULT;

/// Who holds the conversational ball. `Ended` is reserved for an explicit
/// end signal (design §4's Claude Code `SessionEnd` hook, L3) — the
/// transcript-only adapter in this module (§B) can never produce it, since
/// nothing in a Claude Code JSONL transcript records "the session ended";
/// only the hook does.
///
/// LSC-2 — `ts_rs::TS` derive added (purely additive, gated behind the same
/// `ts-export` feature every other wire-facing kb-core enum uses, e.g.
/// `TouchesConfidence`): `routes::sessions::LiveStatusRow` (kb-server)
/// embeds this enum directly rather than re-stringifying it, so the SPA
/// binding generator needs a real `TS` impl to reference.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Holder {
    Agent,
    Human,
    /// Wire value `"none"` (design §5's holder vocabulary is
    /// `agent | human | none`) — `Ended` is the honest Rust name for what
    /// the wire calls the absence of a current holder.
    #[serde(rename = "none")]
    Ended,
}

/// The six lanes [`derive_state`] can return. `PresumedEnded` is reserved
/// for L2 (the daemon's in-memory beat-lease registry: no beat AND no
/// landed capture for a long window) — nothing in this pure function, which
/// only ever sees `Holder::Ended` as a fact fed to it, can derive it yet.
/// LSC-2's `POST /sessions/beat` intake reuses `derive_state` verbatim
/// (does not re-derive state logic), so this variant stays unreachable from
/// the registry too in this phase — see `kb-server/src/live_registry.rs`'s
/// module docs.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveState {
    Working,
    Stalled,
    Waiting,
    Cold,
    Finished,
    PresumedEnded,
}

/// How we know what we know — never decoration (design §5): it's what lets
/// the UI/CLI tell the truth about a session it learned about from a stale
/// capture rather than a live signal.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StateSource {
    /// A harness push hook fired (L2/L3 — not produced by this module).
    Hook,
    /// Pulled by reading the harness's own transcript file directly — what
    /// [`classify_claude_transcript`] produces.
    Transcript,
    /// Rebuilt from a landed, durable capture row on daemon boot (L2 — not
    /// produced by this module).
    Capture,
}

/// [`StateSource`] → confidence is a fixed mapping (design §5), not an
/// independent input — see [`derive_state`].
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Observed,
    Inferred,
    Presumed,
}

/// The two ratified thresholds (design §3, `constants::STALL_AFTER_SECS` /
/// `constants::COLD_AFTER_SECS`) — a struct rather than bare constants so
/// [`derive_state`] stays a pure function of its arguments (testable with
/// synthetic thresholds, never reaching for the ambient constant itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivePolicy {
    /// Seconds an `Agent`-held session may sit silent before it escalates
    /// from `working` to `stalled` (never to `waiting` — see module docs).
    pub stall_after_secs: i64,
    /// Seconds a `Human`-held session may sit silent before it escalates
    /// from `waiting` to `cold`.
    pub cold_after_secs: i64,
    /// Seconds an `Agent`-held session may sit silent before we stop
    /// asserting it is in progress at all and report `presumed_ended`. See
    /// [`ABANDON_AFTER_SECS`] for why the stall horizon alone isn't enough.
    pub abandon_after_secs: i64,
}

impl Default for LivePolicy {
    /// The ratified defaults from `constants.rs`.
    fn default() -> Self {
        Self {
            stall_after_secs: STALL_AFTER_SECS,
            cold_after_secs: COLD_AFTER_SECS,
            abandon_after_secs: ABANDON_AFTER_SECS,
        }
    }
}

/// One live session row — the CLI's (and, later, the wire's) unit of
/// display. Field set matches the LSC-1 work order's illustrative struct
/// with two DOCUMENTED deviations, both additive (nothing was dropped or
/// renamed):
///
/// - `why` — not in the illustrative list, but rule 3 of the Claude Code
///   adapter (§4) explicitly requires "keep the two user kinds
///   distinguishable in a why/debug field" and §C's CLI spec asks for a
///   rendered "why/debug tag" column — there is nowhere else honest to put
///   this than on the row itself.
/// - `version` — §B's "metadata to extract while tailing" lists it
///   explicitly; dropping it after parsing it would be silent data loss for
///   no reason.
///
/// This struct's serde derive uses its own field names, NOT the design
/// §5 wire contract's shape verbatim (`since` is a single ISO-8601 string
/// there; here it's the pair `since_unix`/`since_secs`, exactly as A's type
/// list specifies) — reconciling to the exact `kb-live/1` wire shape is an
/// L2 concern for whatever type the `beat`/`live-status` HTTP routes
/// eventually serialize, not this pure/offline module's job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LiveSession {
    pub session_id: String,
    pub harness: String,
    pub holder: Holder,
    pub state: LiveState,
    pub source: StateSource,
    pub confidence: Confidence,
    /// Unix seconds of the last known activity, clamped to `<= now_unix`
    /// (adapter rule 4 — a writer mid-append can stamp the file's mtime
    /// ahead of our clock).
    pub since_unix: i64,
    /// `(now_unix - since_unix).max(0)` — always non-negative.
    pub since_secs: i64,
    pub project: Option<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub title: Option<String>,
    pub transcript_path: PathBuf,
    pub resume: String,
    /// See the struct doc's "documented deviations" — the debug tag
    /// distinguishing e.g. `stop_reason=tool_use, tail=tool_result` from
    /// `stop_reason=end_turn`.
    pub why: Option<String>,
    /// See the struct doc's "documented deviations" — the harness client
    /// version string, when present on the tailed records.
    pub version: Option<String>,
}

/// THE core pure function (design §3). Two independent axes in, one lane +
/// confidence out:
///
/// - `Holder::Agent`, silent `< stall_after_secs` → `Working`
/// - `Holder::Agent`, silent `>= stall_after_secs` → `Stalled` — **never**
///   `Waiting`: the transcript says the agent has the turn, and silence is
///   not evidence to the contrary (a long-running tool call is silent by
///   construction).
/// - `Holder::Agent`, silent `>= abandon_after_secs` → `PresumedEnded`.
///   The "silence is not evidence" rule above has a horizon: no tool call
///   runs for a working day, so past it the honest reading is that the
///   process died without writing a close. Reported as `PresumedEnded`
///   rather than `Finished` because it is an INFERENCE from a missing
///   signal, not an observed end.
/// - `Holder::Human`, silent `< cold_after_secs` → `Waiting`
/// - `Holder::Human`, silent `>= cold_after_secs` → `Cold`
/// - `Holder::Ended` → `Finished`, regardless of silence.
///
/// `confidence` is a fixed function of `source` (design §5): `Hook` →
/// `Observed`, `Transcript` → `Inferred`, `Capture` → `Presumed`.
///
/// Silence is clamped to `>= 0` internally (`last_activity_unix` may be
/// slightly ahead of `now_unix` — the same mid-append clock skew rule 4 of
/// the Claude Code adapter guards against) so a caller that doesn't
/// pre-clamp its own timestamp still gets an honest `Working`/`Waiting`
/// rather than an underflowed threshold comparison.
pub fn derive_state(
    holder: Holder,
    last_activity_unix: i64,
    now_unix: i64,
    source: StateSource,
    policy: &LivePolicy,
) -> (LiveState, Confidence) {
    let confidence = match source {
        StateSource::Hook => Confidence::Observed,
        StateSource::Transcript => Confidence::Inferred,
        StateSource::Capture => Confidence::Presumed,
    };
    let silent_secs = (now_unix - last_activity_unix).max(0);
    let state = match holder {
        Holder::Ended => LiveState::Finished,
        Holder::Agent => {
            if silent_secs >= policy.abandon_after_secs {
                LiveState::PresumedEnded
            } else if silent_secs >= policy.stall_after_secs {
                LiveState::Stalled
            } else {
                LiveState::Working
            }
        }
        Holder::Human => {
            if silent_secs >= policy.cold_after_secs {
                LiveState::Cold
            } else {
                LiveState::Waiting
            }
        }
    };
    (state, confidence)
}

// --- B. The Claude Code transcript adapter ----------------------------------

/// Which kind of "newest conversational record" the tail ended on — the
/// discriminator adapter rule 3 needs. Named to match the validated Python
/// spike's `last_typed_kind` strings 1:1 (`why` formatting below quotes
/// these verbatim), not kept as an internal-only enum with different
/// spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailKind {
    Assistant,
    ToolResult,
    UserText,
}

impl TailKind {
    fn as_str(self) -> &'static str {
        match self {
            TailKind::Assistant => "assistant",
            TailKind::ToolResult => "tool_result",
            TailKind::UserText => "user_prompt",
        }
    }
}

/// Adapter rule 4, extracted as its own pure function so it's unit-testable
/// without touching the filesystem or a real mtime: a writer mid-append can
/// stamp a file's mtime slightly ahead of our own clock (observed on this
/// box), so the reported "last activity" instant is never allowed to be
/// later than `now_unix`.
///
/// `pub(crate)` (LSC-5) — every sibling harness adapter under
/// `super::live_adapters` clamps its own file-mtime `since_unix` the same
/// way; visibility widened only, behavior untouched.
pub(crate) fn clamp_last_activity_unix(mtime_unix: i64, now_unix: i64) -> i64 {
    mtime_unix.min(now_unix)
}

/// Read at most [`LIVE_TAIL_BYTES`] from the END of `path`, discarding a
/// partial leading line if the read landed mid-file (mirrors the validated
/// Python spike's `tail_records`: `seek(size - n); readline()` to discard).
/// Returns `Some(vec![])` for an empty file (never an error) — the
/// robustness contract [`classify_claude_transcript`] depends on. Trailing
/// content — including a torn, still-being-written final line — is
/// returned as-is; a torn line simply fails to parse as JSON later and is
/// silently skipped, exactly like any other unparseable line.
///
/// `pub(crate)` (LSC-5) — the sibling harness adapters under
/// `super::live_adapters` reuse this exact bounded-tail discipline rather
/// than reimplementing it; visibility widened only, behavior untouched.
pub(crate) fn read_tail_lines(path: &Path) -> Option<Vec<String>> {
    let size = fs::metadata(path).ok()?.len();
    let start = size.saturating_sub(LIVE_TAIL_BYTES);
    let mut f = fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((size - start) as usize);
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if start > 0 && !lines.is_empty() {
        // We seeked into the middle of the file — the first "line" is a
        // fragment of whatever record straddles the window boundary.
        lines.remove(0);
    }
    Some(
        lines
            .into_iter()
            .map(str::to_string)
            .filter(|l| !l.trim().is_empty())
            .collect(),
    )
}

/// Classify one Claude Code transcript file into a [`LiveSession`], reading
/// at most [`LIVE_TAIL_BYTES`] from its end (never a full parse — see the
/// module docs' scale rationale). `None` when the file is empty, contains
/// no `assistant` record anywhere in the tail window, is pure JSON garbage,
/// or ends on an assistant record whose `stop_reason` is neither `tool_use`
/// nor a recognised terminal reason with nothing conversational after it —
/// every one of those is an honest "we don't know" rather than a guess.
///
/// Four rules, each verified against real transcripts on this box and each
/// independently unit-tested below (design §4/§12):
///
/// 1. The ball-holder oracle is the LAST record with `type:"assistant"`'s
///    `message.stop_reason` — NOT whatever the physically-last line of the
///    file happens to be (very often `system`/`attachment`/`last-prompt`/
///    `ai-title`/`mode`/`permission-mode`/an `atis-latch` or
///    `queue-operation` record, none of which carry a stop_reason at all).
/// 2. `stop_reason == "tool_use"` → `Holder::Agent`. `"end_turn"` /
///    `"stop_sequence"` / `"max_tokens"` → `Holder::Human` — unless rule 3
///    overrides.
/// 3. A `type:"user"` record is ambiguous on its own; the content block
///    type discriminates it: a `tool_result` block means the agent is
///    feeding itself mid-turn (→ `Holder::Agent`); a `text` block — or any
///    non-list `content` shape, e.g. the local-command-caveat plain-string
///    records observed on this box — means the HUMAN just spoke (also →
///    `Holder::Agent`: the agent now owes a reply). So whenever the newest
///    *conversational* record (assistant or user) is a `user` record of
///    EITHER kind, the holder is `Agent`; only a trailing `assistant`
///    record with a terminal `stop_reason` and nothing conversational after
///    it means `Human`. The two `user` kinds stay distinguishable in
///    [`LiveSession::why`] (`tail=tool_result` vs. `tail=user_prompt`) —
///    they are different facts.
/// 4. `since_secs` is clamped to `>= 0` via [`clamp_last_activity_unix`].
pub fn classify_claude_transcript(
    path: &Path,
    now_unix: i64,
    policy: &LivePolicy,
) -> Option<LiveSession> {
    let meta = fs::metadata(path).ok()?;
    let mtime_unix = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(now_unix);
    let last_activity_unix = clamp_last_activity_unix(mtime_unix, now_unix);

    let lines = read_tail_lines(path)?;

    let mut seen_assistant = false;
    let mut last_assistant_stop = String::new();
    let mut last_kind: Option<TailKind> = None;
    let mut title: Option<String> = None;
    let mut last_prompt: Option<String> = None;
    let mut model: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut version: Option<String> = None;

    for line in &lines {
        // A torn/partial final line (the writer is mid-append right now),
        // or any other unparseable line, is skipped silently — never an
        // error, never a panic.
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match rec.get("type").and_then(Value::as_str).unwrap_or("") {
            "assistant" => {
                seen_assistant = true;
                let msg = rec.get("message");
                last_assistant_stop = msg
                    .and_then(|m| m.get("stop_reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if let Some(m) = msg.and_then(|m| m.get("model")).and_then(Value::as_str) {
                    model = Some(m.to_string());
                }
                last_kind = Some(TailKind::Assistant);
            }
            "user" => {
                // Rule 3's discriminator: only a `content` array containing
                // a `tool_result`-typed block counts as the agent feeding
                // itself. Everything else (a plain string, a `text` block,
                // an array with no `tool_result`) is the human speaking.
                let is_tool_result = rec
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array)
                    .is_some_and(|blocks| {
                        blocks
                            .iter()
                            .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    });
                last_kind = Some(if is_tool_result {
                    TailKind::ToolResult
                } else {
                    TailKind::UserText
                });
            }
            "ai-title" => {
                if let Some(t) = rec
                    .get("aiTitle")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    title = Some(t.to_string());
                }
            }
            "last-prompt" => {
                if let Some(p) = rec
                    .get("lastPrompt")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    last_prompt = Some(p.to_string());
                }
            }
            _ => {}
        }
        // `cwd`/`version` ride nearly every record type (top-level fields
        // on the envelope, not nested under `message`) — take the newest
        // non-empty value seen, regardless of record type.
        if let Some(c) = rec
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            cwd = Some(c.to_string());
        }
        if let Some(v) = rec
            .get("version")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            version = Some(v.to_string());
        }
    }

    // Rule 1's "no assistant record" honest-None case.
    if !seen_assistant {
        return None;
    }

    let agent_holds = last_assistant_stop == "tool_use"
        || matches!(
            last_kind,
            Some(TailKind::ToolResult) | Some(TailKind::UserText)
        );

    let (holder, why) = if agent_holds {
        let reason = if last_assistant_stop.is_empty() {
            "none"
        } else {
            last_assistant_stop.as_str()
        };
        (
            Holder::Agent,
            format!(
                "stop_reason={reason}, tail={}",
                last_kind.map(TailKind::as_str).unwrap_or("none"),
            ),
        )
    } else if matches!(
        last_assistant_stop.as_str(),
        "end_turn" | "stop_sequence" | "max_tokens"
    ) {
        (Holder::Human, format!("stop_reason={last_assistant_stop}"))
    } else {
        // An assistant record was seen, but its stop_reason is neither
        // "tool_use" nor a recognised terminal reason, and nothing
        // conversational followed it (rule 3 didn't fire either) —
        // genuinely ambiguous. Honest "don't know" rather than a guess.
        return None;
    };

    let (state, confidence) = derive_state(
        holder,
        last_activity_unix,
        now_unix,
        StateSource::Transcript,
        policy,
    );

    // Invariant #11: the canonical session id IS the file stem.
    let session_id = path.file_stem()?.to_str()?.to_string();

    let project = cwd
        .as_deref()
        .and_then(|c| Path::new(c).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| {
            path.parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
        });

    let title = title.or_else(|| last_prompt.map(|p| p.chars().take(70).collect::<String>()));

    Some(LiveSession {
        resume: format!("claude -r {session_id}"),
        session_id,
        harness: HARNESS_DEFAULT.to_string(),
        holder,
        state,
        source: StateSource::Transcript,
        confidence,
        since_unix: last_activity_unix,
        since_secs: (now_unix - last_activity_unix).max(0),
        project,
        cwd,
        model,
        title,
        transcript_path: path.to_path_buf(),
        why: Some(why),
        version,
    })
}

// --- Discovery ---------------------------------------------------------------

/// Bounded discovery walk cap — mirrors kb-server's `PRESENCE_SCAN_CAP`
/// (`routes/sessions.rs`, LF-1): a global cap over every directory entry
/// considered across every project subdirectory, not a per-directory limit.
/// The design's measured rationale is sharper here than for presence: 999
/// subagent/workflow files nested beneath 55 real sessions on this box —
/// this scan's DEPTH-2-ONLY walk already excludes almost all of them by
/// construction, and this cap is the belt-and-suspenders backstop.
pub const LIVE_SCAN_CAP: usize = 4096;

/// Walk `<root>/<project-slug>/<session-uuid>.jsonl` at DEPTH 2 ONLY and
/// classify every candidate via [`classify_claude_transcript`]. Two
/// discipline rules, both load-bearing:
///
/// - **Never descends** into `<root>/<slug>/<session-id>/subagents/**` — a
///   real project directory on this box holds BOTH `<uuid>.jsonl` files
///   (the main transcripts) AND `<uuid>/` subdirectories (containing
///   `subagents/*.jsonl`) side by side; this walk lists a project
///   directory's immediate entries only and skips anything that is itself
///   a directory, so it can never open a subagent or workflow transcript
///   (999 of them on this box, vs. 55 real sessions — see the design's
///   measured evidence).
/// - **Cheap gate first**: `max_age_secs` is checked against the file's
///   mtime, from the SAME `read_dir` metadata already in hand, BEFORE the
///   file is ever opened — a session untouched for a week (or however
///   stale the caller considers "cannot possibly be live") is never read.
///
/// Bounded by [`LIVE_SCAN_CAP`] directory entries total (jsonl or not),
/// across the whole walk — never per-subdirectory.
// `scanned` accumulates across BOTH loop levels (a global cap over every
// project subdirectory), so clippy's "use enumerate" suggestion doesn't
// apply — it would reset the count at each project boundary instead of
// capping the whole walk (same rationale as kb-server's
// `scan_live_presence`, which this mirrors).
#[allow(clippy::explicit_counter_loop)]
pub fn scan_claude_projects(
    root: &Path,
    now_unix: i64,
    policy: &LivePolicy,
    max_age_secs: i64,
) -> Vec<LiveSession> {
    let mut out = Vec::new();
    let mut scanned = 0usize;
    let Ok(project_dirs) = fs::read_dir(root) else {
        return out;
    };
    'outer: for proj in project_dirs.flatten() {
        let proj_path = proj.path();
        if !proj_path.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(&proj_path) else {
            continue;
        };
        for f in files.flatten() {
            if scanned >= LIVE_SCAN_CAP {
                break 'outer;
            }
            scanned += 1;
            let p = f.path();
            // DEPTH 2 ONLY — never descend into a per-session subdirectory
            // (that's where `subagents/*.jsonl` lives).
            if p.is_dir() {
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = f.metadata() else { continue };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let mtime_unix = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            // Cheap gate BEFORE opening the file.
            if now_unix - mtime_unix > max_age_secs {
                continue;
            }
            if let Some(session) = classify_claude_transcript(&p, now_unix, policy) {
                out.push(session);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    /// Real wall-clock now, for the `scan_claude_projects` tests ONLY:
    /// those exercise the cheap age-gate, which compares the caller's
    /// `now_unix` against a freshly-written file's REAL mtime — an
    /// arbitrary fixed `now_unix` (as the `classify_claude_transcript`
    /// tests use, where only the clamp-to-`now` behavior matters, not the
    /// gate) would make `now_unix - mtime_unix` wildly negative and the
    /// age gate vacuously permissive regardless of `max_age_secs`.
    fn real_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    // --- derive_state ---------------------------------------------------

    #[test]
    fn agent_working_when_silent_is_under_stall_threshold() {
        let policy = LivePolicy::default();
        let (state, conf) = derive_state(
            Holder::Agent,
            1_000,
            1_000 + 100,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(state, LiveState::Working);
        assert_eq!(conf, Confidence::Inferred);
    }

    /// THE rule that matters (design §3): silence never flips the holder
    /// axis. A long tool call is still `Stalled`, never `Waiting`.
    #[test]
    fn agent_stalled_past_threshold_is_never_reclassified_as_waiting() {
        let policy = LivePolicy::default();
        let now = 1_000_000;
        let (state, _) = derive_state(
            Holder::Agent,
            now - policy.stall_after_secs - 1,
            now,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(state, LiveState::Stalled);
        assert_ne!(state, LiveState::Waiting);
    }

    /// The stall horizon (LSC-2 amendment). "Silence is not evidence the
    /// agent gave up the turn" holds for a build, not for a week: the first
    /// five-harness run found grok sessions whose last record was
    /// `turn_started` six days earlier pinned to the top of IN PROGRESS.
    #[test]
    fn agent_silent_past_the_abandon_horizon_is_presumed_ended_not_stalled() {
        let policy = LivePolicy::default();
        let now = 1_000_000;
        let (state, _) = derive_state(
            Holder::Agent,
            now - (6 * 86_400),
            now,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(state, LiveState::PresumedEnded);
        assert_ne!(state, LiveState::Stalled);
        // PresumedEnded is an INFERENCE from a missing close; Finished
        // requires an observed end signal. They must never collapse.
        assert_ne!(state, LiveState::Finished);
    }

    #[test]
    fn agent_abandon_boundary_is_inclusive_and_one_second_under_is_still_stalled() {
        let policy = LivePolicy::default();
        let now = 1_000_000;
        let (at, _) = derive_state(
            Holder::Agent,
            now - policy.abandon_after_secs,
            now,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(at, LiveState::PresumedEnded);
        let (under, _) = derive_state(
            Holder::Agent,
            now - policy.abandon_after_secs + 1,
            now,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(under, LiveState::Stalled);
    }

    /// An explicit `end` beat is `Finished` no matter how old — the
    /// abandon horizon only ever applies to an agent-held turn.
    #[test]
    fn ended_holder_is_finished_even_past_the_abandon_horizon() {
        let policy = LivePolicy::default();
        let now = 1_000_000;
        let (state, _) = derive_state(
            Holder::Ended,
            now - (30 * 86_400),
            now,
            StateSource::Hook,
            &policy,
        );
        assert_eq!(state, LiveState::Finished);
    }

    #[test]
    fn agent_stalled_boundary_is_inclusive_at_exactly_the_threshold() {
        let policy = LivePolicy::default();
        let now = 1_000_000;
        let (state, _) = derive_state(
            Holder::Agent,
            now - policy.stall_after_secs,
            now,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(state, LiveState::Stalled);
    }

    #[test]
    fn agent_one_second_under_the_stall_boundary_is_still_working() {
        let policy = LivePolicy::default();
        let now = 1_000_000;
        let (state, _) = derive_state(
            Holder::Agent,
            now - policy.stall_after_secs + 1,
            now,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(state, LiveState::Working);
    }

    #[test]
    fn human_waiting_when_silent_is_under_cold_threshold() {
        let policy = LivePolicy::default();
        let (state, conf) = derive_state(
            Holder::Human,
            1_000,
            1_000 + 100,
            StateSource::Hook,
            &policy,
        );
        assert_eq!(state, LiveState::Waiting);
        assert_eq!(conf, Confidence::Observed);
    }

    #[test]
    fn human_cold_past_the_cold_threshold() {
        let policy = LivePolicy::default();
        let now = 1_000_000;
        let (state, _) = derive_state(
            Holder::Human,
            now - policy.cold_after_secs - 1,
            now,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(state, LiveState::Cold);
    }

    #[test]
    fn human_cold_boundary_is_inclusive_at_exactly_the_threshold() {
        let policy = LivePolicy::default();
        let now = 1_000_000;
        let (state, _) = derive_state(
            Holder::Human,
            now - policy.cold_after_secs,
            now,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(state, LiveState::Cold);
    }

    #[test]
    fn ended_holder_is_always_finished_regardless_of_silence() {
        let policy = LivePolicy::default();
        let (state_fresh, _) =
            derive_state(Holder::Ended, 1_000, 1_000, StateSource::Hook, &policy);
        let (state_old, _) = derive_state(Holder::Ended, 0, 10_000_000, StateSource::Hook, &policy);
        assert_eq!(state_fresh, LiveState::Finished);
        assert_eq!(state_old, LiveState::Finished);
    }

    #[test]
    fn confidence_is_a_fixed_function_of_source() {
        let policy = LivePolicy::default();
        let (_, hook) = derive_state(Holder::Human, 0, 0, StateSource::Hook, &policy);
        let (_, transcript) = derive_state(Holder::Human, 0, 0, StateSource::Transcript, &policy);
        let (_, capture) = derive_state(Holder::Human, 0, 0, StateSource::Capture, &policy);
        assert_eq!(hook, Confidence::Observed);
        assert_eq!(transcript, Confidence::Inferred);
        assert_eq!(capture, Confidence::Presumed);
    }

    #[test]
    fn silence_is_clamped_never_negative_inside_derive_state() {
        let policy = LivePolicy::default();
        // last_activity is AHEAD of now (mid-append clock skew) — must read
        // as zero silence (Working), never underflow/panic.
        let (state, _) = derive_state(
            Holder::Agent,
            2_000,
            1_000,
            StateSource::Transcript,
            &policy,
        );
        assert_eq!(state, LiveState::Working);
    }

    // --- rule 4: clamp_last_activity_unix --------------------------------

    #[test]
    fn rule4_mtime_ahead_of_clock_is_clamped_to_now() {
        assert_eq!(clamp_last_activity_unix(2_000, 1_000), 1_000);
        assert_eq!(clamp_last_activity_unix(500, 1_000), 500);
    }

    // --- classify_claude_transcript: rule 1 ------------------------------

    /// Rule 1: the last physical LINE of the file is very often NOT an
    /// assistant record — this must not be read as "no signal"; the last
    /// record with type:"assistant" is the oracle regardless of what comes
    /// after it.
    #[test]
    fn rule1_uses_last_assistant_record_not_the_physically_last_line() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"assistant","message":{"stop_reason":"tool_use","model":"claude-fable-5"}}"#,
            "\n",
            r#"{"type":"system","subtype":"info"}"#,
            "\n",
            r#"{"type":"ai-title","aiTitle":"Doing the thing"}"#,
            "\n",
            r#"{"type":"queue-operation","op":"drain"}"#,
            "\n",
        );
        let p = write_file(tmp.path(), "sid-rule1.jsonl", content);
        let policy = LivePolicy::default();
        let got = classify_claude_transcript(&p, 100_000_000, &policy).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert_eq!(got.state, LiveState::Working);
        assert_eq!(got.title.as_deref(), Some("Doing the thing"));
    }

    // --- classify_claude_transcript: rule 2 ------------------------------

    #[test]
    fn rule2_tool_use_stop_reason_is_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let content = r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}
"#;
        let p = write_file(tmp.path(), "sid-r2a.jsonl", content);
        let got = classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
    }

    #[test]
    fn rule2_end_turn_stop_sequence_max_tokens_are_all_human() {
        for reason in ["end_turn", "stop_sequence", "max_tokens"] {
            let tmp = tempfile::tempdir().unwrap();
            let content = format!(
                r#"{{"type":"assistant","message":{{"stop_reason":"{reason}"}}}}
"#
            );
            let p = write_file(tmp.path(), "sid-r2b.jsonl", &content);
            let got = classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).unwrap();
            assert_eq!(got.holder, Holder::Human, "stop_reason={reason}");
        }
    }

    // --- classify_claude_transcript: rule 3 ------------------------------

    /// Rule 3: a `tool_result` user record after an `end_turn` assistant
    /// record means the agent is still mid-turn — Agent, not Human.
    #[test]
    fn rule3_tool_result_after_end_turn_flips_holder_back_to_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            "\n",
        );
        let p = write_file(tmp.path(), "sid-r3a.jsonl", content);
        let got = classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert!(got.why.as_deref().unwrap().contains("tail=tool_result"));
    }

    /// Rule 3: a plain `text` user record after an `end_turn` assistant
    /// record means the HUMAN just spoke — which still means Agent (it now
    /// owes a reply), and the why-tag must say `user_prompt`, not
    /// `tool_result` — they are different facts.
    #[test]
    fn rule3_fresh_human_text_after_end_turn_is_also_agent_but_tagged_differently() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"text","text":"go on"}]}}"#,
            "\n",
        );
        let p = write_file(tmp.path(), "sid-r3b.jsonl", content);
        let got = classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert!(got.why.as_deref().unwrap().contains("tail=user_prompt"));
    }

    /// The plain-string `content` shape observed on this box (e.g. the
    /// local-command-caveat records) must be treated as human text, not
    /// crash on the `as_array()` mismatch.
    #[test]
    fn rule3_plain_string_content_is_treated_as_human_text_not_tool_result() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"<local-command-caveat>hi</local-command-caveat>"}}"#,
            "\n",
        );
        let p = write_file(tmp.path(), "sid-r3c.jsonl", content);
        let got = classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
        assert!(got.why.as_deref().unwrap().contains("tail=user_prompt"));
    }

    /// No trailing user record at all: the assistant's own terminal
    /// stop_reason stands — Human.
    #[test]
    fn rule3_no_trailing_user_record_leaves_the_assistant_stop_reason_authoritative() {
        let tmp = tempfile::tempdir().unwrap();
        let content = r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}
"#;
        let p = write_file(tmp.path(), "sid-r3d.jsonl", content);
        let got = classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Human);
        assert_eq!(got.why.as_deref(), Some("stop_reason=end_turn"));
    }

    // --- classify_claude_transcript: rule 4 (since_secs clamp) -----------

    #[test]
    fn rule4_since_secs_on_the_returned_session_is_never_negative() {
        let tmp = tempfile::tempdir().unwrap();
        let content = r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}
"#;
        let p = write_file(tmp.path(), "sid-r4.jsonl", content);
        // now_unix far in the past relative to the file's real (fresh) mtime
        // — since_unix must clamp down to now_unix, so since_secs is 0.
        let now_in_the_past = 1;
        let got = classify_claude_transcript(&p, now_in_the_past, &LivePolicy::default()).unwrap();
        assert!(got.since_secs >= 0);
        assert_eq!(got.since_unix, now_in_the_past);
        assert_eq!(got.since_secs, 0);
    }

    // --- robustness -------------------------------------------------------

    #[test]
    fn robustness_empty_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(tmp.path(), "sid-empty.jsonl", "");
        assert!(classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_no_assistant_record_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let content = concat!(
            r#"{"type":"user","message":{"content":"hello"}}"#,
            "\n",
            r#"{"type":"system","subtype":"info"}"#,
            "\n",
        );
        let p = write_file(tmp.path(), "sid-nouser.jsonl", content);
        assert!(classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_pure_garbage_returns_none_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write_file(
            tmp.path(),
            "sid-garbage.jsonl",
            "not json at all\n{{{\nrandom bytes",
        );
        assert!(classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_ambiguous_stop_reason_with_no_trailing_user_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        // A stop_reason outside the closed {tool_use, end_turn,
        // stop_sequence, max_tokens} set, and nothing conversational after
        // it — genuinely ambiguous, must not guess.
        let content = r#"{"type":"assistant","message":{"stop_reason":"refusal"}}
"#;
        let p = write_file(tmp.path(), "sid-ambiguous.jsonl", content);
        assert!(classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).is_none());
    }

    #[test]
    fn robustness_torn_trailing_line_is_skipped_not_errored() {
        let tmp = tempfile::tempdir().unwrap();
        let mut content = String::new();
        content.push_str(r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}"#);
        content.push('\n');
        // A torn/partial line — the writer is mid-append. No trailing
        // newline: this must be skipped silently, and the valid record
        // above must still classify successfully.
        content.push_str(r#"{"type":"assistant","message":{"stop_reason":"end_"#);
        let p = write_file(tmp.path(), "sid-torn.jsonl", &content);
        let got = classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.holder, Holder::Agent);
    }

    /// Never full-parses: a valid record placed BEFORE the
    /// [`LIVE_TAIL_BYTES`] window must be invisible to the classifier —
    /// only the tail window's own state is honoured.
    #[test]
    fn tail_window_bounds_the_read_to_at_most_live_tail_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut content = String::new();
        // A title that would win if the whole file were read.
        content.push_str(r#"{"type":"ai-title","aiTitle":"STALE — outside the window"}"#);
        content.push('\n');
        // Padding well past LIVE_TAIL_BYTES so the line above falls outside
        // the tail window entirely.
        let filler_line = format!(r#"{{"type":"system","pad":"{}"}}"#, "x".repeat(4000));
        let filler_lines_needed = (LIVE_TAIL_BYTES as usize) / (filler_line.len() + 1) + 4;
        for _ in 0..filler_lines_needed {
            content.push_str(&filler_line);
            content.push('\n');
        }
        content.push_str(r#"{"type":"ai-title","aiTitle":"fresh — inside the window"}"#);
        content.push('\n');
        content.push_str(r#"{"type":"assistant","message":{"stop_reason":"tool_use"}}"#);
        content.push('\n');
        let p = write_file(tmp.path(), "sid-window.jsonl", &content);
        assert!(fs::metadata(&p).unwrap().len() > LIVE_TAIL_BYTES);
        let got = classify_claude_transcript(&p, 100_000_000, &LivePolicy::default()).unwrap();
        assert_eq!(got.title.as_deref(), Some("fresh — inside the window"));
    }

    // --- scan_claude_projects ---------------------------------------------

    #[test]
    fn scan_finds_the_depth2_transcript_and_never_descends_into_subagents() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("-home-user-project-kb");
        fs::create_dir_all(&proj).unwrap();
        write_file(
            &proj,
            "real-session.jsonl",
            "{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"tool_use\"}}\n",
        );
        // A per-session subagent nest, sibling to the real transcript —
        // must never be opened or returned.
        let sub_dir = proj.join("real-session").join("subagents");
        fs::create_dir_all(&sub_dir).unwrap();
        write_file(
            &sub_dir,
            "agent-1.jsonl",
            "{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"tool_use\"}}\n",
        );
        let got = scan_claude_projects(tmp.path(), real_now(), &LivePolicy::default(), 86_400);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].session_id, "real-session");
    }

    #[test]
    fn scan_cheap_gate_skips_a_too_old_file_before_opening_it() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("-proj");
        fs::create_dir_all(&proj).unwrap();
        write_file(
            &proj,
            "s.jsonl",
            "{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"tool_use\"}}\n",
        );
        // A negative max_age means "older than -1 seconds old" — since a
        // freshly-written file's age is >= 0, this excludes it via the
        // cheap gate deterministically without needing to fake an old
        // mtime.
        let now = real_now();
        let excluded = scan_claude_projects(tmp.path(), now, &LivePolicy::default(), -1);
        assert!(excluded.is_empty(), "{excluded:?}");
        let included = scan_claude_projects(tmp.path(), now, &LivePolicy::default(), 86_400);
        assert_eq!(included.len(), 1);
    }

    #[test]
    fn scan_respects_the_global_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("-cap");
        fs::create_dir_all(&proj).unwrap();
        let n = LIVE_SCAN_CAP + 64;
        for i in 0..n {
            write_file(
                &proj,
                &format!("s{i}.jsonl"),
                "{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"tool_use\"}}\n",
            );
        }
        let got = scan_claude_projects(tmp.path(), real_now(), &LivePolicy::default(), 86_400);
        assert!(
            got.len() <= LIVE_SCAN_CAP,
            "cap {LIVE_SCAN_CAP} exceeded: got {}",
            got.len()
        );
        assert_eq!(got.len(), LIVE_SCAN_CAP);
    }

    #[test]
    fn scan_returns_empty_for_a_nonexistent_root_not_an_error_or_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let got = scan_claude_projects(&missing, 100_000_000, &LivePolicy::default(), 86_400);
        assert!(got.is_empty());
    }
}
