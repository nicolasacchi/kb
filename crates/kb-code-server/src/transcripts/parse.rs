//! One transcript JSONL LINE → zero or more indexable [`ParsedTurn`]s.
//!
//! # Real shapes (grounded against live `~/.claude/projects/**/*.jsonl`)
//!
//! Every line is a JSON object whose `type` field discriminates the shape:
//! `"user"` / `"assistant"` (indexed — see below), and a long tail this
//! module deliberately does NOT index by simply never matching it in
//! [`parse_line`]'s `match`: `"attachment"`, `"system"` (hook output —
//! `hookAdditionalContext`/`hookCount`/`hookErrors`/`hookInfos` fields),
//! `"file-history-snapshot"` (env-dump risk — a full workspace file
//! snapshot), `"last-prompt"`, `"mode"`, `"permission-mode"`, `"ai-title"`,
//! `"queue-operation"`, and anything else. Not matching them is the
//! exclusion mechanism — there is no separate deny-list to keep in sync
//! with Claude Code's own evolving line vocabulary.
//!
//! `"user"`/`"assistant"` lines share an envelope: `uuid`, `parentUuid`
//! (absent/null for a session's root turn), `sessionId`, `timestamp`
//! (ISO-8601), `isSidechain` (bool, default `false` if the field is ever
//! absent). `message.content` is EITHER a plain string (the common case for
//! a `"user"` line — direct human text) OR a list of content blocks (an
//! `"assistant"` line always; a `"user"` line when it's carrying tool
//! results back to the model). Block `type` values seen: `"text"`,
//! `"thinking"` (+ `signature`), `"tool_use"` (+ `name`, `input` object),
//! `"tool_result"` (+ `tool_use_id`, `is_error`, `content` — itself either a
//! plain string or a list of `{type, text}` blocks).
//!
//! # Extraction rules
//!
//! One line can yield MULTIPLE turns — an `assistant` line with a thinking
//! block, a text block, and a tool_use block produces three. Per block:
//!
//! - `"user"` line, string content → one `kind = "user"` turn, `text` =
//!   the string verbatim.
//! - `"user"` line, list content, `tool_result` block → one
//!   `kind = "tool_result"` turn per block, `text` capped at
//!   [`TOOL_RESULT_CAP_BYTES`] (a tool result can be arbitrarily large —
//!   a full file read, a long command's stdout — and this is a full-text
//!   INDEX, not a transcript viewer; the raw JSONL is still the source of
//!   truth for anyone who needs the untruncated bytes). Other block types
//!   in a user-authored list (none observed in the wild as of this
//!   grounding pass) are skipped.
//! - `"assistant"` line, `text` block → one `kind = "assistant"` turn.
//! - `"assistant"` line, `thinking` block → one `kind = "thinking"` turn,
//!   ONLY when `index_thinking` is `true` (the `[transcripts]
//!   index_thinking` config toggle, default `true` — a raw chain-of-thought
//!   dump is exactly the kind of content an operator might want indexed
//!   for "what was I thinking when I did X" recall, but it's also the
//!   noisiest/largest content in a transcript, hence the toggle).
//! - `"assistant"` line, `tool_use` block → one `kind = "tool_use"` turn.
//!   `tool_name` = the block's `name`. `text` = `"{name} "` followed by a
//!   handful of SELECTED input values likely to be useful search terms
//!   (`command`, `query`, `pattern`, `description`, plus the same
//!   path-shaped keys [`extract_file_paths`] pulls out) — not a dump of
//!   the entire `input` object (some tool inputs carry large payloads,
//!   e.g. `Write`'s `content`, which would bloat the index for little
//!   search value). [`extract_file_paths`] separately extracts
//!   `file_path`/`path`/`notebook_path` values into the turn's own
//!   `file_paths` list.
//!
//! Every other line `type` (including `"attachment"` and
//! `"file-history-snapshot"`) yields an empty `Vec` — see the module doc's
//! opening paragraph.

use serde_json::Value;

/// Cap on a `tool_result` turn's indexed text — see the module doc.
pub const TOOL_RESULT_CAP_BYTES: usize = 2000;

/// The five `transcript_turns.kind` values (schema-pinned — `V0003`'s
/// `CHECK (kind IN (...))` uses exactly this vocabulary).
pub const KIND_USER: &str = "user";
pub const KIND_ASSISTANT: &str = "assistant";
pub const KIND_THINKING: &str = "thinking";
pub const KIND_TOOL_USE: &str = "tool_use";
pub const KIND_TOOL_RESULT: &str = "tool_result";

/// One indexable turn extracted from a transcript line. Carries no byte
/// offset/length of its own — [`super::indexer`] (which already tracks the
/// line's position while walking the file) attaches
/// `byte_offset`/`byte_len` for the WHOLE LINE after parsing it (see the
/// migration's doc on why every turn from one line shares that range).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedTurn {
    pub session_id: String,
    pub uuid: String,
    pub parent_uuid: Option<String>,
    /// Unix milliseconds, parsed from the envelope's ISO-8601 `timestamp`.
    pub ts: i64,
    pub kind: &'static str,
    pub tool_name: Option<String>,
    pub file_paths: Vec<String>,
    pub is_sidechain: bool,
    pub text: String,
}

/// Parse one JSONL line into zero or more turns. A line that isn't valid
/// JSON, or whose `type` this module doesn't index, yields an empty `Vec`
/// — never an error: [`super::indexer`]'s tail walk must tolerate a
/// partially-written or exotic line without aborting the whole file.
pub fn parse_line(line: &str, index_thinking: bool) -> Vec<ParsedTurn> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(type_) = value.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };

    match type_ {
        "user" => parse_user(&value),
        "assistant" => parse_assistant(&value, index_thinking),
        _ => Vec::new(),
    }
}

/// Shared envelope fields every indexed turn from this line will carry.
struct Envelope {
    session_id: String,
    uuid: String,
    parent_uuid: Option<String>,
    ts: i64,
    is_sidechain: bool,
}

fn envelope(value: &Value) -> Option<Envelope> {
    let session_id = value.get("sessionId")?.as_str()?.to_string();
    let uuid = value.get("uuid")?.as_str()?.to_string();
    let parent_uuid = value
        .get("parentUuid")
        .and_then(Value::as_str)
        .map(str::to_string);
    let ts = value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0);
    let is_sidechain = value
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(Envelope {
        session_id,
        uuid,
        parent_uuid,
        ts,
        is_sidechain,
    })
}

fn parse_user(value: &Value) -> Vec<ParsedTurn> {
    let Some(env) = envelope(value) else {
        return Vec::new();
    };
    let Some(content) = value.pointer("/message/content") else {
        return Vec::new();
    };

    match content {
        Value::String(s) => vec![turn(&env, KIND_USER, None, Vec::new(), s.clone())],
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                    return None;
                }
                let text = tool_result_text(block);
                Some(turn(
                    &env,
                    KIND_TOOL_RESULT,
                    None,
                    Vec::new(),
                    safe_truncate(&text, TOOL_RESULT_CAP_BYTES),
                ))
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A `tool_result` block's own `content` is either a plain string or a list
/// of `{type: "text", text: "..."}`-shaped blocks — concatenate the latter.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn parse_assistant(value: &Value, index_thinking: bool) -> Vec<ParsedTurn> {
    let Some(env) = envelope(value) else {
        return Vec::new();
    };
    let Some(Value::Array(blocks)) = value.pointer("/message/content") else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    out.push(turn(
                        &env,
                        KIND_ASSISTANT,
                        None,
                        Vec::new(),
                        text.to_string(),
                    ));
                }
            }
            Some("thinking") if index_thinking => {
                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                    out.push(turn(
                        &env,
                        KIND_THINKING,
                        None,
                        Vec::new(),
                        text.to_string(),
                    ));
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                let input = block.get("input");
                let file_paths = input.map(extract_file_paths).unwrap_or_default();
                let text = tool_use_text(name, input);
                out.push(turn(
                    &env,
                    KIND_TOOL_USE,
                    Some(name.to_string()),
                    file_paths,
                    text,
                ));
            }
            _ => {}
        }
    }
    out
}

/// `"{name} key=value ..."` over a small, deliberately curated set of input
/// keys likely to carry search-worthy free text — see the module doc's
/// tool_use bullet for why this isn't a dump of the whole `input` object.
const TOOL_USE_TEXT_KEYS: &[&str] = &[
    "command",
    "query",
    "pattern",
    "description",
    "file_path",
    "path",
    "notebook_path",
    "prompt",
    "url",
];

fn tool_use_text(name: &str, input: Option<&Value>) -> String {
    let mut parts = vec![name.to_string()];
    if let Some(Value::Object(map)) = input {
        for key in TOOL_USE_TEXT_KEYS {
            if let Some(v) = map.get(*key).and_then(Value::as_str) {
                parts.push(v.to_string());
            }
        }
    }
    parts.join(" ")
}

/// Pull path-shaped values out of a tool_use `input` object — the turn's
/// `file_paths` list (`transcript_turns.file_paths`, JSON-encoded by the
/// caller). Deliberately a small fixed key set rather than a heuristic
/// "looks like a path" scan over every string value, to avoid false
/// positives from e.g. a `Bash` command string that happens to contain a
/// `/`.
const FILE_PATH_KEYS: &[&str] = &["file_path", "path", "notebook_path"];

fn extract_file_paths(input: &Value) -> Vec<String> {
    let Value::Object(map) = input else {
        return Vec::new();
    };
    FILE_PATH_KEYS
        .iter()
        .filter_map(|key| map.get(*key).and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn turn(
    env: &Envelope,
    kind: &'static str,
    tool_name: Option<String>,
    file_paths: Vec<String>,
    text: String,
) -> ParsedTurn {
    ParsedTurn {
        session_id: env.session_id.clone(),
        uuid: env.uuid.clone(),
        parent_uuid: env.parent_uuid.clone(),
        ts: env.ts,
        kind,
        tool_name,
        file_paths,
        is_sidechain: env.is_sidechain,
        text,
    }
}

/// Truncate `s` to at most `max_bytes` bytes WITHOUT splitting a multi-byte
/// UTF-8 char — a plain `&s[..max_bytes]` panics if `max_bytes` lands
/// mid-codepoint, which a fixed byte cap over arbitrary tool output
/// (unicode box-drawing characters in a `tree` dump, emoji in a commit
/// message, ...) can easily hit.
fn safe_truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_line(content_blocks: &str) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"u1","parentUuid":"p1","sessionId":"s1","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{{"role":"assistant","content":[{content_blocks}]}}}}"#
        )
    }

    #[test]
    fn assistant_line_with_text_thinking_tool_use_yields_three_turns() {
        let line = assistant_line(
            r#"{"type":"thinking","thinking":"pondering the bug","signature":"sig"},
               {"type":"text","text":"Here is the fix."},
               {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test","description":"run tests"}}"#,
        );
        let turns = parse_line(&line, true);
        assert_eq!(turns.len(), 3, "got {turns:#?}");

        let thinking = turns.iter().find(|t| t.kind == KIND_THINKING).unwrap();
        assert_eq!(thinking.text, "pondering the bug");
        assert_eq!(thinking.session_id, "s1");
        assert_eq!(thinking.uuid, "u1");
        assert_eq!(thinking.parent_uuid.as_deref(), Some("p1"));
        assert!(!thinking.is_sidechain);
        assert_eq!(thinking.ts, 1784282400000);

        let text = turns.iter().find(|t| t.kind == KIND_ASSISTANT).unwrap();
        assert_eq!(text.text, "Here is the fix.");
        assert!(text.tool_name.is_none());

        let tool_use = turns.iter().find(|t| t.kind == KIND_TOOL_USE).unwrap();
        assert_eq!(tool_use.tool_name.as_deref(), Some("Bash"));
        assert!(tool_use.text.contains("Bash"));
        assert!(tool_use.text.contains("cargo test"));
        assert!(tool_use.text.contains("run tests"));
        assert!(tool_use.file_paths.is_empty(), "Bash has no path input");
    }

    #[test]
    fn index_thinking_false_drops_thinking_blocks_only() {
        let line = assistant_line(
            r#"{"type":"thinking","thinking":"secret plan","signature":"sig"},
               {"type":"text","text":"done"}"#,
        );
        let turns = parse_line(&line, false);
        assert_eq!(turns.len(), 1, "got {turns:#?}");
        assert_eq!(turns[0].kind, KIND_ASSISTANT);
    }

    #[test]
    fn tool_use_extracts_file_paths_from_curated_keys() {
        let line = assistant_line(
            r#"{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"/home/user/project/kb/src/lib.rs","old_string":"a","new_string":"b"}}"#,
        );
        let turns = parse_line(&line, true);
        assert_eq!(turns.len(), 1);
        assert_eq!(
            turns[0].file_paths,
            vec!["/home/user/project/kb/src/lib.rs".to_string()]
        );
        // old_string/new_string are not in the curated text-key set — the
        // tool_use text stays small, not a dump of the whole edit.
        assert!(!turns[0].text.contains("old_string"));
    }

    #[test]
    fn user_line_with_plain_string_content_yields_one_user_turn() {
        let line = r#"{"type":"user","uuid":"u2","parentUuid":null,"sessionId":"s1","timestamp":"2026-07-17T10:00:01.000Z","isSidechain":false,"message":{"role":"user","content":"what does this function do?"}}"#;
        let turns = parse_line(line, true);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].kind, KIND_USER);
        assert_eq!(turns[0].text, "what does this function do?");
        assert!(turns[0].parent_uuid.is_none(), "root turn has no parent");
    }

    #[test]
    fn user_line_with_tool_result_block_yields_tool_result_turn() {
        let line = r#"{"type":"user","uuid":"u3","parentUuid":"u2","sessionId":"s1","timestamp":"2026-07-17T10:00:02.000Z","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"ok: 3 tests passed"}]}}"#;
        let turns = parse_line(line, true);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].kind, KIND_TOOL_RESULT);
        assert_eq!(turns[0].text, "ok: 3 tests passed");
    }

    #[test]
    fn tool_result_text_is_capped_at_2kb_on_a_char_boundary() {
        // Multi-byte content so the cap lands mid-codepoint if not handled
        // carefully — every char here is a 3-byte UTF-8 sequence ('€').
        let big = "€".repeat(1000); // 3000 bytes
        let line = format!(
            r#"{{"type":"user","uuid":"u4","parentUuid":"u3","sessionId":"s1","timestamp":"2026-07-17T10:00:03.000Z","isSidechain":false,"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","is_error":false,"content":"{big}"}}]}}}}"#
        );
        let turns = parse_line(&line, true);
        assert_eq!(turns.len(), 1);
        assert!(turns[0].text.len() <= TOOL_RESULT_CAP_BYTES);
        assert!(
            turns[0].text.len() > TOOL_RESULT_CAP_BYTES - 4,
            "should truncate close to the cap, not way under it"
        );
        // Every remaining char must still be a valid, whole '€' — proves no
        // codepoint was split.
        assert!(turns[0].text.chars().all(|c| c == '€'));
    }

    #[test]
    fn sidechain_flag_round_trips() {
        let line = r#"{"type":"user","uuid":"u5","parentUuid":null,"sessionId":"s2","timestamp":"2026-07-17T10:00:04.000Z","isSidechain":true,"message":{"role":"user","content":"subagent task"}}"#;
        let turns = parse_line(line, true);
        assert_eq!(turns.len(), 1);
        assert!(turns[0].is_sidechain);
    }

    #[test]
    fn attachment_and_file_history_lines_are_skipped() {
        let attachment = r#"{"type":"attachment","uuid":"a1","parentUuid":null,"sessionId":"s1","timestamp":"2026-07-17T10:00:05.000Z","isSidechain":false,"attachment":{"foo":"bar"}}"#;
        assert!(parse_line(attachment, true).is_empty());

        let file_history = r#"{"type":"file-history-snapshot","messageId":"m1","isSnapshotUpdate":false,"snapshot":{"secret_env":"leaked"}}"#;
        assert!(parse_line(file_history, true).is_empty());

        let system = r#"{"type":"system","uuid":"sy1","parentUuid":null,"sessionId":"s1","timestamp":"2026-07-17T10:00:06.000Z","isSidechain":false,"subtype":"hook","level":"info"}"#;
        assert!(parse_line(system, true).is_empty());
    }

    #[test]
    fn blank_and_malformed_lines_yield_no_turns() {
        assert!(parse_line("", true).is_empty());
        assert!(parse_line("   ", true).is_empty());
        assert!(parse_line("{not valid json", true).is_empty());
        assert!(
            parse_line(r#"{"type":"user"}"#, true).is_empty(),
            "missing envelope fields"
        );
    }
}
