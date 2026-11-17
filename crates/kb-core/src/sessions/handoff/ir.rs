//! The neutral conversation model every handoff reader produces and every
//! renderer/writer consumes. Harness-agnostic on purpose: the omp tree, the
//! Claude uuid chain and kb's Claude-shaped captures all fold into it, so the
//! brief and the transplant writers never look at a source format.
//!
//! What it deliberately does NOT carry: provider signatures (thinking
//! signatures, encrypted reasoning, provider payloads), usage/cost
//! accounting, harness-internal state entries (title slots, model/thinking
//! changes, credential pins, file-history snapshots, hook attachments) and
//! subagent sidechains. None of them is portable across harnesses.

use serde::Serialize;
use serde_json::Value;

/// The two harnesses a handoff moves between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    Claude,
    Omp,
}

impl Harness {
    /// The lowercase wire/CLI token (`"claude" | "omp"`), the same token
    /// [`crate::sessions::resume::resume_command`] keys on.
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Omp => "omp",
        }
    }

    /// Parse a wire/CLI token. Exact lowercase match only.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Harness::Claude),
            "omp" => Some(Harness::Omp),
            _ => None,
        }
    }

    /// The other harness (the default handoff target).
    pub fn other(self) -> Self {
        match self {
            Harness::Claude => Harness::Omp,
            Harness::Omp => Harness::Claude,
        }
    }
}

/// Where a [`Conversation`] was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// The harness's own on-disk session file (full fidelity).
    Raw,
    /// A kb capture's recovered transcript. For omp-origin sessions this is
    /// kb's lossy Claude-shaped translation (leaf chain only, tool results
    /// capped at 2000 chars, no images).
    Capture,
}

/// Which source-authored summary a [`Summary`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryOrigin {
    /// An omp `compaction` entry (any `method` except `handoff`).
    OmpCompaction,
    /// An omp `compaction` entry with `method: "handoff"` (`/handoff`).
    OmpHandoff,
    /// A Claude Code `isCompactSummary` user record (`/compact` or
    /// auto-compact).
    ClaudeCompact,
    /// The `[compaction] …` synthetic assistant message kb's omp capture
    /// translator emits in place of a compaction entry.
    CaptureCompaction,
}

/// The newest LLM-authored summary the SOURCE harness already wrote on the
/// live chain. kb never generates one (the daemon is LLM-free).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub origin: SummaryOrigin,
    pub text: String,
    /// ISO-8601 UTC time the summary was written, when known.
    pub at: Option<String>,
    /// How many leading [`Conversation::turns`] the summary stands for.
    /// The harness itself replaced `turns[..covers]` with this summary in
    /// its own context; a transplant does the same. `0` when the reader
    /// could not recover the pre-summary turns.
    pub covers: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One content block. User turns carry `Text`, `Image` and `ToolResult`;
/// assistant turns carry `Text`, `Thinking` and `ToolCall`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    /// Readable reasoning text only. Provider signatures are never carried;
    /// writers drop these blocks (a foreign or missing signature is an API
    /// error risk), the brief ignores them.
    Thinking {
        text: String,
    },
    ToolCall {
        /// The source harness's call id, verbatim (may be invalid for the
        /// target; writers run it through [`super::tools::safe_tool_id`]).
        id: String,
        /// The source harness's tool name, verbatim (`bash`, `Read`, `edit` …).
        name: String,
        /// Always a JSON object. Readers parse string-encoded arguments and
        /// wrap unparsable ones as `{"_raw": "<string>"}`.
        input: Value,
    },
    ToolResult {
        call_id: String,
        /// The tool name, when the source records it or the reader could
        /// resolve it from the matching call.
        name: Option<String>,
        /// Text parts joined with `\n`; non-text parts are dropped (images in
        /// tool results are not carried).
        text: String,
        is_error: bool,
    },
    Image {
        /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
        media_type: String,
        /// Base64 payload, no `data:` prefix.
        data: String,
    },
}

/// One message in chronological order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Turn {
    pub role: Role,
    /// ISO-8601 UTC, when the source recorded one.
    pub timestamp: Option<String>,
    /// Assistant turns: the model that produced it, as the source recorded it
    /// (`claude-opus-5-5`, `anthropic/claude-opus-5-5`, `openrouter/…`).
    pub model: Option<String>,
    pub blocks: Vec<Block>,
}

impl Turn {
    /// A real typed prompt: a user turn with at least one non-blank text
    /// block (a user turn that only carries tool results is not a prompt).
    pub fn is_prompt(&self) -> bool {
        self.role == Role::User
            && self
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Text { text } if !text.trim().is_empty()))
    }

    /// The concatenated text blocks (`\n\n`-joined), ignoring every other
    /// block kind.
    pub fn text(&self) -> String {
        let parts: Vec<&str> = self
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
                _ => None,
            })
            .collect();
        parts.join("\n\n")
    }
}

/// A session's live conversation, harness-neutral.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conversation {
    /// The harness that produced the transcript.
    pub harness: Harness,
    /// The source session id (omp header `id`, Claude `sessionId`).
    pub session_id: String,
    pub source_kind: SourceKind,
    /// Absolute path of the file read (raw session file or capture).
    pub source_path: Option<String>,
    /// The session's working directory (omp header `cwd`; Claude: the last
    /// record's `cwd`).
    pub cwd: Option<String>,
    /// The freshest human/AI title the source recorded.
    pub title: Option<String>,
    pub git_branch: Option<String>,
    /// The model of the last assistant turn, as recorded.
    pub model: Option<String>,
    /// ISO-8601 UTC of the first and last turn.
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    /// The newest source-authored summary on the live chain, if any.
    pub summary: Option<Summary>,
    /// The live chain in chronological order: abandoned branches are not
    /// included, and nothing before an omp `/clear` (`reset_boundary`) is.
    /// Includes the turns a [`Summary`] covers when the reader recovered
    /// them (see [`Summary::covers`]).
    pub turns: Vec<Turn>,
    /// Honesty notes for the operator: lossy source, skipped lines,
    /// unresolved blobs, dropped entry kinds.
    pub notes: Vec<String>,
}

impl Conversation {
    /// Every real typed prompt, in order (see [`Turn::is_prompt`]).
    pub fn prompts(&self) -> impl Iterator<Item = &Turn> {
        self.turns.iter().filter(|t| t.is_prompt())
    }
}
