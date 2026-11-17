//! Cross-harness session handoff (omp ⇄ Claude Code), `kb-handoff/1`.
//!
//! One neutral conversation model ([`ir::Conversation`]), two products:
//!
//! * a **brief** ([`brief::render_brief`], the default): deterministic,
//!   LLM-free text a NEW session of the target harness receives as its first
//!   prompt, on request (`kb sessions handoff`, the session page);
//! * a **transplant** ([`transplant::plan_transplant`] +
//!   [`claude::write_claude_transcript`] / [`omp::write_omp_session`]): a
//!   native session file the target resumes with `claude -r` / `omp -r`,
//!   holding a summary preamble plus the most recent turns verbatim.
//!
//! Readers: [`omp::read_omp_session`] (the raw omp tree) and
//! [`claude::read_claude_jsonl`] (a raw Claude transcript, or a kb capture's
//! Claude-shaped JSONL — the daemon's only source). Everything is pure except
//! the `locate_*` helpers, which only list directories; nothing here writes a
//! file, callers place what the writers return.
//!
//! Every brief and every transplant preamble starts with [`marker_line`]; the
//! session parser reads it back ([`parse_marker`]) as the target session's
//! `continued_from` lineage (surfaced, never scored).

pub mod brief;
pub mod claude;
pub mod ir;
pub mod omp;
pub mod tools;
pub mod transplant;

pub use ir::{Block, Conversation, Harness, Role, SourceKind, Summary, SummaryOrigin, Turn};

/// Schema tag for every handoff wire payload.
pub const HANDOFF_SCHEMA: &str = "kb-handoff/1";

/// Why a reader could not produce a [`Conversation`].
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    /// The input is not a session transcript of the expected harness.
    #[error("not a {harness} session transcript: {reason}")]
    NotASession {
        harness: &'static str,
        reason: String,
    },
    /// The transcript parsed but its live chain holds no user or assistant
    /// turn.
    #[error("session {session_id} has no conversation turns")]
    Empty { session_id: String },
}

const MARKER_OPEN: &str = "[kb-handoff from ";
const MARKER_SESSION: &str = " session ";

/// How far into a first prompt [`parse_marker`] looks.
const MARKER_SCAN_CHARS: usize = 1024;

/// The provenance line that opens every brief and transplant preamble:
/// `[kb-handoff from omp session 01a1e4a1-…]`.
pub fn marker_line(source: Harness, session_id: &str) -> String {
    format!(
        "{MARKER_OPEN}{}{MARKER_SESSION}{session_id}]",
        source.as_str()
    )
}

fn parse_marker_line(line: &str) -> Option<(Harness, String)> {
    let rest = line.trim().strip_prefix(MARKER_OPEN)?.strip_suffix(']')?;
    let (harness, sid) = rest.split_once(MARKER_SESSION)?;
    let harness = Harness::parse(harness)?;
    if !crate::sessions::resume::is_plain_token(sid) {
        return None;
    }
    Some((harness, sid.to_string()))
}

/// Read a handoff marker back from the start of a session's first prompt.
/// Looks at whole lines within the first 1024 characters (a harness may
/// prepend a line of its own); the first marker line wins. The session id
/// must be a plain token, so a forged marker cannot smuggle shell or markup.
pub fn parse_marker(text: &str) -> Option<(Harness, String)> {
    let end = text
        .char_indices()
        .nth(MARKER_SCAN_CHARS)
        .map_or(text.len(), |(i, _)| i);
    let window = &text[..end];
    let complete = if end < text.len() {
        // Drop a line the window cut in half.
        window.rfind('\n').map_or("", |i| &window[..i])
    } else {
        window
    };
    complete.lines().find_map(parse_marker_line)
}

/// `text` without its first handoff marker line (the preview a session list
/// shows should be the prompt, not the provenance line).
pub fn strip_marker_line(text: &str) -> String {
    let mut out = Vec::new();
    let mut stripped = false;
    for line in text.lines() {
        if !stripped && parse_marker_line(line).is_some() {
            stripped = true;
            continue;
        }
        out.push(line);
    }
    if !stripped {
        return text.to_string();
    }
    out.join("\n").trim_start_matches('\n').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_round_trips_and_rejects_forgeries() {
        let sid = "01a1e4a1-0d74-7548-9455-e37ffe71b0a1";
        let line = marker_line(Harness::Omp, sid);
        assert_eq!(line, format!("[kb-handoff from omp session {sid}]"));
        assert_eq!(
            parse_marker(&format!("{line}\nrest of the brief")),
            Some((Harness::Omp, sid.to_string()))
        );
        // A harness-added first line does not hide it.
        assert_eq!(
            parse_marker(&format!("<file>\n{line}\n")),
            Some((Harness::Omp, sid.to_string()))
        );
        assert_eq!(
            parse_marker("[kb-handoff from omp session $(rm -rf ~)]"),
            None
        );
        assert_eq!(parse_marker("[kb-handoff from codex session abc]"), None);
        assert_eq!(parse_marker("no marker here"), None);
        // Past the scan window it is not a first-prompt marker.
        let late = format!("{}\n{line}", "x".repeat(MARKER_SCAN_CHARS));
        assert_eq!(parse_marker(&late), None);
    }

    #[test]
    fn strip_marker_line_removes_only_the_marker() {
        let text = format!("{}\n\nGoal: ship it", marker_line(Harness::Claude, "abc-1"));
        assert_eq!(strip_marker_line(&text), "Goal: ship it");
        assert_eq!(strip_marker_line("plain prompt"), "plain prompt");
    }
}
