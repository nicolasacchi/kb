//! The **transplant** plan: what a native session file for the target harness
//! holds — a preamble (the first user message: marker, provenance, the source
//! summary or a deterministic brief) plus the most recent turns verbatim.
//!
//! Pure and format-agnostic. The writers ([`super::claude::write_claude_transcript`],
//! [`super::omp::write_omp_session`]) own every target-format rule: tool
//! policy ([`super::tools::plan_tool_call`]), id sanitising, strict
//! user/assistant alternation, per-record timestamps.

use super::brief::{render_brief, summary_origin_label, BriefOptions};
use super::ir::{Block, Conversation, Harness, Role, Turn};
use super::tools::clip_middle;

/// How a transplant is planned.
#[derive(Debug, Clone, PartialEq)]
pub struct TransplantOptions {
    pub target: Harness,
    /// The new session's id (see [`new_session_id`]).
    pub new_session_id: String,
    /// Absolute working directory of the new session.
    pub cwd: String,
    /// ISO-8601 UTC, used for synthetic timestamps and `created_at`.
    pub now: String,
    /// Rough cap on the verbatim tail (characters of text, tool input and
    /// clipped tool output).
    pub tail_budget_chars: usize,
    /// Every tool result is clipped to this many characters.
    pub tool_result_max_chars: usize,
    pub target_model: Option<String>,
}

impl TransplantOptions {
    pub const DEFAULT_TAIL_BUDGET_CHARS: usize = 200_000;
    pub const DEFAULT_TOOL_RESULT_MAX_CHARS: usize = 20_000;
}

/// A planned transplant, ready for a writer.
#[derive(Debug, Clone, PartialEq)]
pub struct TransplantPlan {
    pub source: Harness,
    pub source_session_id: String,
    pub target: Harness,
    pub new_session_id: String,
    pub cwd: String,
    pub title: Option<String>,
    /// `= opts.now`.
    pub created_at: String,
    /// The FIRST user message; starts with the handoff marker line.
    pub preamble: String,
    /// The verbatim tail: no thinking blocks, tool results clipped, the first
    /// turn is a prompt or an assistant turn (never a user turn holding
    /// orphan tool results).
    pub turns: Vec<Turn>,
    /// Live-chain turns not carried verbatim.
    pub omitted_turns: usize,
    pub target_model: Option<String>,
}

/// Brief budget for a transplant preamble when the source has no summary of
/// its own (no per-message injection cap applies to a session file).
const PREAMBLE_BRIEF_BUDGET_CHARS: usize = 12_000;

const TAIL_SENTENCE: &str =
    "The messages after this are the most recent part of that session, verbatim.";

/// Estimated size of a turn once carried (thinking dropped, results clipped).
fn turn_size(turn: &Turn, tool_result_max_chars: usize) -> usize {
    turn.blocks
        .iter()
        .map(|b| match b {
            Block::Text { text } => text.chars().count(),
            Block::Thinking { .. } => 0,
            Block::ToolCall { name, input, .. } => {
                name.chars().count() + input.to_string().chars().count()
            }
            Block::ToolResult { text, .. } => text.chars().count().min(tool_result_max_chars),
            Block::Image { data, .. } => data.len(),
        })
        .sum()
}

/// The turn as it is carried: thinking dropped, tool results clipped. `None`
/// when nothing is left.
fn carried(turn: &Turn, tool_result_max_chars: usize) -> Option<Turn> {
    let blocks: Vec<Block> = turn
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Thinking { .. } => None,
            Block::ToolResult {
                call_id,
                name,
                text,
                is_error,
            } => Some(Block::ToolResult {
                call_id: call_id.clone(),
                name: name.clone(),
                text: clip_middle(text, tool_result_max_chars),
                is_error: *is_error,
            }),
            other => Some(other.clone()),
        })
        .collect();
    if blocks.is_empty() {
        return None;
    }
    Some(Turn {
        role: turn.role,
        timestamp: turn.timestamp.clone(),
        model: turn.model.clone(),
        blocks,
    })
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn preamble(conv: &Conversation, opts: &TransplantOptions, has_tail: bool) -> String {
    let src = conv.harness.as_str();
    let mut out = super::marker_line(conv.harness, &conv.session_id);
    out.push_str("\n\n");
    match conv.summary.as_ref().filter(|s| !s.text.trim().is_empty()) {
        Some(summary) => {
            let mut about = format!("This session continues {src} session {}", conv.session_id);
            if let Some(title) = conv
                .title
                .as_deref()
                .map(one_line)
                .filter(|t| !t.is_empty())
            {
                about.push_str(&format!(" (\"{title}\")"));
            }
            if let Some(cwd) = conv.cwd.as_deref().filter(|c| !c.is_empty()) {
                about.push_str(&format!(", started in {cwd}"));
            }
            out.push_str(&about);
            out.push_str(&format!(
                ". It was handed off from {src} to {}; what follows is that session's record and \
                 may be stale, so verify it against git and the files before acting on it.\n\n",
                opts.target.as_str()
            ));
            out.push_str(&format!(
                "## Summary from the source session ({})\n\n{}",
                summary_origin_label(summary.origin),
                summary.text.trim()
            ));
        }
        None => {
            // The brief opens with the same provenance paragraph, header
            // fields and stale-record warning, so it stands alone here.
            let brief = render_brief(
                conv,
                &BriefOptions {
                    target: opts.target,
                    budget_chars: PREAMBLE_BRIEF_BUDGET_CHARS,
                    tail_turns: 0,
                },
            );
            out.push_str(super::strip_marker_line(&brief.text).trim_end());
        }
    }
    if has_tail {
        out.push_str("\n\n");
        out.push_str(TAIL_SENTENCE);
    }
    out
}

/// Plan the transplant of `conv` into a new `opts.target` session.
pub fn plan_transplant(conv: &Conversation, opts: &TransplantOptions) -> TransplantPlan {
    let covered = conv
        .summary
        .as_ref()
        .map_or(0, |s| s.covers)
        .min(conv.turns.len());
    let candidates = &conv.turns[covered..];

    // Newest-first until the estimated size exceeds the budget.
    let mut used = 0usize;
    let mut start = candidates.len();
    for (i, turn) in candidates.iter().enumerate().rev() {
        let size = turn_size(turn, opts.tool_result_max_chars);
        if used + size > opts.tail_budget_chars {
            break;
        }
        used += size;
        start = i;
    }

    let mut turns: Vec<Turn> = candidates[start..]
        .iter()
        .filter_map(|t| carried(t, opts.tool_result_max_chars))
        .collect();
    // Never open on a user turn that holds tool results whose calls are gone.
    let first_ok = turns
        .iter()
        .position(|t| t.role == Role::Assistant || t.is_prompt())
        .unwrap_or(turns.len());
    turns.drain(..first_ok);

    let omitted_turns = conv.turns.len() - turns.len();
    TransplantPlan {
        source: conv.harness,
        source_session_id: conv.session_id.clone(),
        target: opts.target,
        new_session_id: opts.new_session_id.clone(),
        cwd: opts.cwd.clone(),
        title: conv.title.clone(),
        created_at: opts.now.clone(),
        preamble: preamble(conv, opts, !turns.is_empty()),
        turns,
        omitted_turns,
        target_model: opts.target_model.clone(),
    }
}

/// Lowercase hyphenated UUID text of 16 bytes (8-4-4-4-12).
fn format_uuid(b: &[u8; 16]) -> String {
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// 16 random bytes; falls back to a clock-seeded xorshift when the OS RNG is
/// unavailable (an id only has to be unique, not secret).
fn random_bytes() -> [u8; 16] {
    let mut buf = [0u8; 16];
    if getrandom::fill(&mut buf).is_ok() {
        return buf;
    }
    let mut x = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        ^ u64::from(std::process::id()).rotate_left(32)
        | 1;
    for byte in buf.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *byte = (x >> 24) as u8;
    }
    buf
}

/// A UUID v4 (random) or, with `unix_ms`, a v7 (48-bit unix-ms prefix) from
/// `rand`; version nibble and RFC 4122 variant bits set.
fn build_uuid(mut b: [u8; 16], unix_ms: Option<u64>) -> String {
    let version = match unix_ms {
        Some(ms) => {
            b[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
            0x70
        }
        None => 0x40,
    };
    b[6] = (b[6] & 0x0f) | version;
    b[8] = (b[8] & 0x3f) | 0x80;
    format_uuid(&b)
}

/// A fresh session id for `target`: Claude → UUID v4, omp → UUID v7 (omp's
/// own ids are time-ordered).
pub fn new_session_id(target: Harness) -> String {
    match target {
        Harness::Claude => build_uuid(random_bytes(), None),
        Harness::Omp => {
            let ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
            build_uuid(random_bytes(), Some(ms))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::handoff::ir::{SourceKind, Summary, SummaryOrigin};
    use serde_json::json;

    fn conv(turns: Vec<Turn>) -> Conversation {
        Conversation {
            harness: Harness::Omp,
            session_id: "01a1e4a1-0d74-7548-9455-e37ffe71b0a1".to_string(),
            source_kind: SourceKind::Raw,
            source_path: None,
            cwd: Some("/work/proj".to_string()),
            title: Some("Port it".to_string()),
            git_branch: None,
            model: None,
            started_at: None,
            ended_at: None,
            summary: None,
            turns,
            notes: Vec::new(),
        }
    }

    fn opts() -> TransplantOptions {
        TransplantOptions {
            target: Harness::Claude,
            new_session_id: "new-id".to_string(),
            cwd: "/work/proj".to_string(),
            now: "2026-11-16T12:00:00Z".to_string(),
            tail_budget_chars: TransplantOptions::DEFAULT_TAIL_BUDGET_CHARS,
            tool_result_max_chars: TransplantOptions::DEFAULT_TOOL_RESULT_MAX_CHARS,
            target_model: Some("claude-opus-5-5".to_string()),
        }
    }

    fn user(text: &str) -> Turn {
        Turn {
            role: Role::User,
            timestamp: None,
            model: None,
            blocks: vec![Block::Text {
                text: text.to_string(),
            }],
        }
    }

    fn assistant(blocks: Vec<Block>) -> Turn {
        Turn {
            role: Role::Assistant,
            timestamp: None,
            model: None,
            blocks,
        }
    }

    fn text(t: &str) -> Block {
        Block::Text {
            text: t.to_string(),
        }
    }

    fn call(id: &str, command: &str) -> Block {
        Block::ToolCall {
            id: id.to_string(),
            name: "bash".to_string(),
            input: json!({ "command": command }),
        }
    }

    fn result(id: &str, body: &str) -> Turn {
        Turn {
            role: Role::User,
            timestamp: None,
            model: None,
            blocks: vec![Block::ToolResult {
                call_id: id.to_string(),
                name: Some("bash".to_string()),
                text: body.to_string(),
                is_error: false,
            }],
        }
    }

    fn all_text(turns: &[Turn]) -> String {
        turns
            .iter()
            .map(|t| t.text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn summary_covered_turns_never_reach_the_tail() {
        let mut c = conv(vec![
            user("covered prompt"),
            assistant(vec![call("c0", "ls")]),
            result("c0", "covered output"),
            assistant(vec![text("fresh answer")]),
            user("fresh prompt"),
            assistant(vec![call("c1", "pwd")]),
            result("c1", "/work"),
            assistant(vec![text("done")]),
        ]);
        c.summary = Some(Summary {
            origin: SummaryOrigin::OmpCompaction,
            text: "## Goal\nport the parser".to_string(),
            at: None,
            covers: 3,
        });
        let plan = plan_transplant(&c, &opts());
        assert_eq!(plan.turns.len(), 5);
        assert!(!all_text(&plan.turns).contains("covered"));
        assert!(!format!("{:?}", plan.turns).contains("covered output"));
        assert_eq!(plan.omitted_turns, 3);
        assert!(plan
            .preamble
            .starts_with("[kb-handoff from omp session 01a1e4a1-0d74-7548-9455-e37ffe71b0a1]"));
        assert!(plan
            .preamble
            .contains("## Summary from the source session (omp compaction)"));
        assert!(plan.preamble.contains("port the parser"));
        assert!(plan.preamble.ends_with(TAIL_SENTENCE));
        assert_eq!(plan.created_at, "2026-11-16T12:00:00Z");
        assert_eq!(plan.title.as_deref(), Some("Port it"));
        assert_eq!(plan.target_model.as_deref(), Some("claude-opus-5-5"));
    }

    #[test]
    fn budget_cut_never_opens_on_a_tool_result_turn() {
        let c = conv(vec![
            user("q"),
            assistant(vec![call("c0", "ls")]),
            result("c0", &"x".repeat(100)),
            assistant(vec![text("done")]),
        ]);
        let mut o = opts();
        // "done" (4) + the result (100) fit; the call turn (20) does not.
        o.tail_budget_chars = 110;
        let plan = plan_transplant(&c, &o);
        assert_eq!(plan.turns.len(), 1);
        assert_eq!(plan.turns[0].role, Role::Assistant);
        assert_eq!(plan.omitted_turns, 3);
    }

    #[test]
    fn nothing_fits_means_an_empty_tail_and_no_tail_sentence() {
        let c = conv(vec![user("a prompt"), assistant(vec![text("an answer")])]);
        let mut o = opts();
        o.tail_budget_chars = 3;
        let plan = plan_transplant(&c, &o);
        assert!(plan.turns.is_empty());
        assert_eq!(plan.omitted_turns, 2);
        assert!(!plan.preamble.contains(TAIL_SENTENCE));
        // No source summary: the preamble carries the deterministic brief body.
        assert!(plan.preamble.contains("## User prompts (1)"));
        assert_eq!(plan.preamble.matches("[kb-handoff from").count(), 1);
    }

    #[test]
    fn thinking_is_dropped_and_huge_results_are_clipped() {
        let c = conv(vec![
            user("go"),
            assistant(vec![
                Block::Thinking {
                    text: "private reasoning".to_string(),
                },
                text("running"),
                call("c0", "cat big"),
            ]),
            result("c0", &"y".repeat(50_000)),
            assistant(vec![Block::Thinking {
                text: "only thinking".to_string(),
            }]),
        ]);
        let mut o = opts();
        o.tool_result_max_chars = 1000;
        let plan = plan_transplant(&c, &o);
        let dump = format!("{:?}", plan.turns);
        assert!(!dump.contains("private reasoning") && !dump.contains("only thinking"));
        assert_eq!(plan.turns.len(), 3, "the thinking-only turn is gone");
        let Block::ToolResult { text, .. } = &plan.turns[2].blocks[0] else {
            panic!("expected a tool result");
        };
        assert!(text.chars().count() < 1100, "{}", text.chars().count());
        assert!(text.contains("chars omitted"));
        assert_eq!(plan.omitted_turns, 1);
    }

    fn assert_uuid_shape(id: &str) {
        assert_eq!(id.len(), 36, "{id}");
        for (i, c) in id.chars().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                assert_eq!(c, '-', "{id}");
            } else {
                assert!(c.is_ascii_digit() || ('a'..='f').contains(&c), "{id}");
            }
        }
        assert!(
            matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "variant: {id}"
        );
    }

    #[test]
    fn session_ids_are_v4_for_claude_and_time_prefixed_v7_for_omp() {
        let v4 = new_session_id(Harness::Claude);
        assert_uuid_shape(&v4);
        assert_eq!(v4.as_bytes()[14], b'4');
        assert_ne!(v4, new_session_id(Harness::Claude));

        let before = chrono::Utc::now().timestamp_millis() as u64;
        let v7 = new_session_id(Harness::Omp);
        let after = chrono::Utc::now().timestamp_millis() as u64;
        assert_uuid_shape(&v7);
        assert_eq!(v7.as_bytes()[14], b'7');
        let prefix = u64::from_str_radix(&format!("{}{}", &v7[..8], &v7[9..13]), 16).unwrap();
        assert!(
            prefix >= before && prefix <= after,
            "{prefix} not in {before}..={after}"
        );
    }
}
