//! The Claude Code half of the handoff: locate a raw transcript, read a raw
//! transcript (or a kb capture's recovered Claude-shaped JSONL, including the
//! lossy omp-origin ones) into the neutral [`Conversation`], and write a
//! transcript `claude -r <id>` accepts from a [`TransplantPlan`].
//!
//! Reader: Claude persists a branching tree (`uuid`/`parentUuid`), so the live
//! conversation is the chain walked back from the resumed leaf —
//! `last-prompt.leafUuid` when it resolves, else the last main-thread record —
//! and across a `compact_boundary` through its `logicalParentUuid`, so the
//! pre-compaction turns are recovered too (a [`Summary`] then says how many
//! turns it stands for). kb's omp captures carry no uuids at all; those are
//! read in file order.
//!
//! Writer: every rule here exists because `claude -r` (2.1.x) or the API
//! rejects or silently mangles the file otherwise — a `uuid`/`parentUuid`
//! chain (a flat file resumes only its last record), an ISO `timestamp` on
//! every record (a missing or non-ISO one on the tail record is
//! `No conversation found`), tool-call ids matching `^[A-Za-z0-9_-]+$`, no
//! thinking blocks (a foreign signature is a 400 risk), strict user/assistant
//! alternation, and no orphan tool calls or results.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::sessions::chain::{flag, live_order, str_of, type_of};
#[cfg(test)]
use crate::sessions::chain::is_conversation_type;

use super::ir::{Block, Conversation, Harness, Role, SourceKind, Summary, SummaryOrigin, Turn};
use super::tools::{
    flattened_call_text, flattened_result_text, plan_tool_call, safe_tool_id, ToolPlan,
};
use super::transplant::TransplantPlan;
use super::HandoffError;

/// Longest a flattened call line gets in a transplant.
const FLATTEN_CALL_MAX_CHARS: usize = 400;

/// The `tool_result` content written for a native call whose output was
/// empty (the API rejects an empty tool_result). Flattened results are
/// written as the plan holds them: the planner already clipped them, and a
/// second clip would drop its omitted-chars marker and misreport the count.
const EMPTY_RESULT_TEXT: &str = "(no output)";

/// The `tool_result` content written for a call whose result the plan does
/// not hold (the source session ended mid-tool).
const ORPHAN_RESULT_TEXT: &str =
    "[no result recorded: the source session ended before this tool returned]";

/// How much of the preamble the `last-prompt` trailer line carries.
const LAST_PROMPT_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// Locators
// ---------------------------------------------------------------------------

/// Every Claude `projects` directory that exists, in lookup order:
/// `$CLAUDE_CONFIG_DIR/projects` (when given), `<home>/.claude/projects`, then
/// `<home>/.claude-accounts/*/projects` (sorted). Deduped by canonical path,
/// so a config dir that points at `~/.claude` is listed once.
pub fn claude_projects_roots(claude_config_dir: Option<&Path>, home: &Path) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = claude_config_dir {
        candidates.push(dir.join("projects"));
    }
    candidates.push(home.join(".claude").join("projects"));
    let mut accounts: Vec<PathBuf> = std::fs::read_dir(home.join(".claude-accounts"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("projects"))
        .collect();
    accounts.sort();
    candidates.extend(accounts);

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut roots = Vec::new();
    for c in candidates {
        if !c.is_dir() {
            continue;
        }
        let canon = c.canonicalize().unwrap_or_else(|_| c.clone());
        if seen.insert(canon) {
            roots.push(c);
        }
    }
    roots
}

/// Transcript files `<root>/<slug>/<file>.jsonl` whose stem starts with
/// `id_prefix` (case-insensitive), newest mtime first. Exactly one directory
/// level deep: `<slug>/<sid>/subagents/agent-*.jsonl` sidecars are not
/// sessions. Missing roots contribute nothing; an empty prefix matches
/// nothing (it would match every session).
pub fn locate_claude_session(roots: &[PathBuf], id_prefix: &str) -> io::Result<Vec<PathBuf>> {
    let prefix = id_prefix.trim().to_ascii_lowercase();
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for root in roots {
        let buckets = match std::fs::read_dir(root) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for bucket in buckets.flatten() {
            let bucket_path = bucket.path();
            if !bucket_path.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&bucket_path) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(OsStr::to_str) != Some("jsonl") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(OsStr::to_str) else {
                    continue;
                };
                if !stem.to_ascii_lowercase().starts_with(&prefix) {
                    continue;
                }
                let Ok(meta) = file.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let key = path.canonicalize().unwrap_or_else(|_| path.clone());
                if !seen.insert(key) {
                    continue;
                }
                found.push((meta.modified().unwrap_or(UNIX_EPOCH), path));
            }
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    Ok(found.into_iter().map(|(_, p)| p).collect())
}

// ---------------------------------------------------------------------------
// Reader
// ---------------------------------------------------------------------------

/// The record's `timestamp` when it is a real ISO-8601 string (kb captures
/// write `"unknown"` when omp had none).
fn iso_timestamp(raw: Option<&str>) -> Option<String> {
    let t = raw?;
    DateTime::parse_from_rfc3339(t).ok().map(|_| t.to_string())
}

fn non_empty(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Read a raw Claude transcript, or a kb capture's recovered JSONL (including
/// omp-origin captures: first line `adapter-meta` with `harness: "omp"`), into
/// a [`Conversation`].
///
/// A capture from any other harness is [`HandoffError::NotASession`]; a
/// transcript without conversation turns is [`HandoffError::Empty`].
pub fn read_claude_jsonl(
    jsonl: &str,
    source_kind: SourceKind,
    source_path: Option<&str>,
) -> Result<Conversation, HandoffError> {
    let mut records: Vec<Value> = Vec::new();
    let mut unparsable = 0usize;
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) if v.is_object() => records.push(v),
            _ => unparsable += 1,
        }
    }
    if records.is_empty() {
        return Err(HandoffError::NotASession {
            harness: "claude",
            reason: "no JSON object lines".to_string(),
        });
    }

    let meta = records.iter().find(|r| type_of(r) == "adapter-meta");
    let harness = match meta.and_then(|m| str_of(m, "harness")) {
        None | Some("claude") => Harness::Claude,
        Some("omp") => Harness::Omp,
        Some(other) => {
            return Err(HandoffError::NotASession {
                harness: "claude",
                reason: format!("capture is from harness `{other}`"),
            })
        }
    };
    let session_id = meta
        .and_then(|m| non_empty(str_of(m, "sessionId")))
        .or_else(|| {
            records
                .iter()
                .find_map(|r| non_empty(str_of(r, "sessionId")))
        })
        .ok_or_else(|| HandoffError::NotASession {
            harness: "claude",
            reason: "no sessionId on any record".to_string(),
        })?;
    // The id lands in the brief's marker line and in backticked
    // `kb sessions read <id>` pointers, so a crafted one must not get in.
    if !crate::sessions::resume::is_plain_token(&session_id) {
        return Err(HandoffError::NotASession {
            harness: "claude",
            reason: "session id is not a plain token".to_string(),
        });
    }

    let mut notes: Vec<String> = Vec::new();
    if unparsable > 0 {
        notes.push(format!("skipped {unparsable} unparsable line(s)"));
    }
    if let Some(file) = meta.and_then(|m| non_empty(str_of(m, "session_file"))) {
        notes.push(format!("raw file: {file}"));
    }
    if harness == Harness::Omp && source_kind == SourceKind::Capture {
        notes.push(
            "omp-origin capture is lossy: tool results capped at 2000 chars, leaf chain only, \
             no images"
                .to_string(),
        );
    }

    let order = live_order(&records, &mut notes);
    let capture_synthetics = harness == Harness::Omp;

    let mut turns: Vec<Turn> = Vec::new();
    let mut summary: Option<Summary> = None;
    let mut call_names: HashMap<String, String> = HashMap::new();
    let mut last_assistant_id: Option<String> = None;
    let mut last_model: Option<String> = None;

    for &i in &order {
        let r = &records[i];
        if flag(r, "isSidechain") {
            continue;
        }
        let timestamp = iso_timestamp(str_of(r, "timestamp"));
        let content = r.pointer("/message/content");
        match type_of(r) {
            "user" => {
                if flag(r, "isCompactSummary") {
                    summary = Some(Summary {
                        origin: SummaryOrigin::ClaudeCompact,
                        text: content_text(content).trim().to_string(),
                        at: timestamp,
                        covers: turns.len(),
                    });
                    last_assistant_id = None;
                    continue;
                }
                if flag(r, "isMeta") {
                    continue;
                }
                let blocks = user_blocks(content, &call_names);
                if blocks.is_empty() {
                    continue;
                }
                turns.push(Turn {
                    role: Role::User,
                    timestamp,
                    model: None,
                    blocks,
                });
                last_assistant_id = None;
            }
            "assistant" => {
                if flag(r, "isApiErrorMessage") {
                    continue;
                }
                let msg_id = r.pointer("/message/id").and_then(Value::as_str);
                let model = r
                    .pointer("/message/model")
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty() && *m != "<synthetic>")
                    .map(str::to_string);
                let mut blocks: Vec<Block> = Vec::new();
                assistant_blocks(
                    content,
                    capture_synthetics,
                    &timestamp,
                    turns.len(),
                    &mut call_names,
                    &mut summary,
                    &mut blocks,
                );
                if let Some(m) = &model {
                    last_model = Some(m.clone());
                }
                if blocks.is_empty() {
                    continue;
                }
                let merge = msg_id.is_some()
                    && msg_id == last_assistant_id.as_deref()
                    && matches!(turns.last(), Some(t) if t.role == Role::Assistant);
                if merge {
                    if let Some(prev) = turns.last_mut() {
                        prev.blocks.append(&mut blocks);
                        if prev.model.is_none() {
                            prev.model = model;
                        }
                    }
                } else {
                    turns.push(Turn {
                        role: Role::Assistant,
                        timestamp,
                        model,
                        blocks,
                    });
                    last_assistant_id = msg_id.map(str::to_string);
                }
            }
            _ => {}
        }
    }

    if turns.is_empty() {
        return Err(HandoffError::Empty { session_id });
    }

    // Titles: custom-title beats ai-title beats the capture's own slots; the
    // newest line of each kind wins.
    let last_of = |ty: &str, key: &str| {
        records
            .iter()
            .rev()
            .filter(|r| type_of(r) == ty)
            .find_map(|r| non_empty(str_of(r, key)))
    };
    let title = last_of("custom-title", "customTitle")
        .or_else(|| last_of("ai-title", "aiTitle"))
        .or_else(|| meta.and_then(|m| non_empty(str_of(m, "aiTitle"))))
        .or_else(|| meta.and_then(|m| non_empty(str_of(m, "title"))));

    let last_field = |key: &str| records.iter().rev().find_map(|r| non_empty(str_of(r, key)));
    let started_at = turns.iter().find_map(|t| t.timestamp.clone());
    let ended_at = turns.iter().rev().find_map(|t| t.timestamp.clone());

    Ok(Conversation {
        harness,
        session_id,
        source_kind,
        source_path: source_path.map(str::to_string),
        cwd: last_field("cwd"),
        title,
        git_branch: last_field("gitBranch"),
        model: last_model,
        started_at,
        ended_at,
        summary,
        turns,
        notes,
    })
}

/// Text of a message `content`: a string, or the text blocks joined with a
/// newline.
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let texts: Vec<&str> = parts
                .iter()
                .filter(|p| matches!(str_of(p, "type"), Some("text") | None))
                .filter_map(|p| str_of(p, "text"))
                .collect();
            texts.join("\n")
        }
        _ => String::new(),
    }
}

/// A tool call's `input` as an object: objects verbatim, strings parsed as
/// JSON, anything else wrapped as `{"_raw": …}`.
fn tool_input(v: Option<&Value>) -> Value {
    match v {
        Some(o @ Value::Object(_)) => o.clone(),
        None | Some(Value::Null) => json!({}),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(o @ Value::Object(_)) => o,
            _ => json!({ "_raw": s }),
        },
        Some(other) => json!({ "_raw": other.to_string() }),
    }
}

fn user_blocks(content: Option<&Value>, call_names: &HashMap<String, String>) -> Vec<Block> {
    let mut blocks = Vec::new();
    match content {
        Some(Value::String(s)) => {
            if !s.trim().is_empty() {
                blocks.push(Block::Text { text: s.clone() });
            }
        }
        Some(Value::Array(parts)) => {
            for p in parts {
                match type_of(p) {
                    "text" => {
                        if let Some(t) = str_of(p, "text").filter(|t| !t.trim().is_empty()) {
                            blocks.push(Block::Text {
                                text: t.to_string(),
                            });
                        }
                    }
                    "image" => {
                        let src = p.get("source");
                        let media = src.and_then(|s| str_of(s, "media_type"));
                        let data = src.and_then(|s| str_of(s, "data"));
                        let is_base64 = src.and_then(|s| str_of(s, "type")) == Some("base64");
                        if let (true, Some(media), Some(data)) = (is_base64, media, data) {
                            if !data.is_empty() {
                                blocks.push(Block::Image {
                                    media_type: media.to_string(),
                                    data: data.to_string(),
                                });
                            }
                        }
                    }
                    "tool_result" => {
                        let call_id = str_of(p, "tool_use_id").unwrap_or("").to_string();
                        let name = call_names.get(&call_id).cloned();
                        blocks.push(Block::ToolResult {
                            call_id,
                            name,
                            text: content_text(p.get("content")),
                            is_error: flag(p, "is_error"),
                        });
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    blocks
}

/// Append the blocks of one assistant record to `out`. For kb's omp captures
/// (`capture_synthetics`) a `[compaction] …` text becomes the summary
/// (`covers` = turns before it) and a `[session-exit] …` text is dropped.
fn assistant_blocks(
    content: Option<&Value>,
    capture_synthetics: bool,
    timestamp: &Option<String>,
    covers: usize,
    call_names: &mut HashMap<String, String>,
    summary: &mut Option<Summary>,
    out: &mut Vec<Block>,
) {
    let mut push_text = |text: &str, out: &mut Vec<Block>| {
        if capture_synthetics {
            let head = text.trim_start();
            if let Some(rest) = head.strip_prefix("[compaction]") {
                *summary = Some(Summary {
                    origin: SummaryOrigin::CaptureCompaction,
                    text: rest.trim().to_string(),
                    at: timestamp.clone(),
                    covers,
                });
                return;
            }
            if head.starts_with("[session-exit]") {
                return;
            }
        }
        if !text.trim().is_empty() {
            out.push(Block::Text {
                text: text.to_string(),
            });
        }
    };
    match content {
        Some(Value::String(s)) => push_text(s, &mut *out),
        Some(Value::Array(parts)) => {
            for p in parts {
                match type_of(p) {
                    "text" => {
                        if let Some(t) = str_of(p, "text") {
                            push_text(t, &mut *out);
                        }
                    }
                    "thinking" => {
                        if let Some(t) = str_of(p, "thinking").filter(|t| !t.trim().is_empty()) {
                            out.push(Block::Thinking {
                                text: t.to_string(),
                            });
                        }
                    }
                    "tool_use" => {
                        let id = str_of(p, "id").unwrap_or("").to_string();
                        let name = str_of(p, "name").unwrap_or("tool").to_string();
                        call_names.insert(id.clone(), name.clone());
                        out.push(Block::ToolCall {
                            id,
                            name,
                            input: tool_input(p.get("input")),
                        });
                    }
                    // redacted_thinking and anything unknown: not portable.
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// One message of the transcript being built, before it becomes a record.
struct OutMsg {
    role: Role,
    /// Anthropic content blocks as JSON.
    blocks: Vec<Value>,
    /// ISO timestamp of the first turn folded into this message.
    timestamp: Option<String>,
}

/// How a tool call was written, so its result follows the same path.
enum CallPlan {
    Native { safe_id: String },
    Flatten { name: String },
}

fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

fn block_type(b: &Value) -> &str {
    type_of(b)
}

fn is_blank_text(b: &Value) -> bool {
    block_type(b) == "text" && str_of(b, "text").is_none_or(|t| t.trim().is_empty())
}

/// Drop blank text blocks and empty messages, then merge adjacent same-role
/// messages (assistant blocks concatenate in order; user content likewise).
fn normalize(msgs: &mut Vec<OutMsg>) {
    let mut out: Vec<OutMsg> = Vec::with_capacity(msgs.len());
    for mut m in msgs.drain(..) {
        m.blocks.retain(|b| !is_blank_text(b));
        if m.blocks.is_empty() {
            continue;
        }
        if let Some(prev) = out.last_mut().filter(|p| p.role == m.role) {
            prev.blocks.append(&mut m.blocks);
            continue;
        }
        out.push(m);
    }
    *msgs = out;
}

fn tool_use_ids(m: &OutMsg) -> Vec<String> {
    m.blocks
        .iter()
        .filter(|b| block_type(b) == "tool_use")
        .filter_map(|b| str_of(b, "id").map(str::to_string))
        .collect()
}

/// Make tool pairing airtight: every `tool_use` gets exactly one result in
/// the user message right after it (a synthetic error result when the plan
/// has none), and a `tool_result` that does not answer a `tool_use` of the
/// message before it is dropped. Returns whether anything was dropped (the
/// caller then re-normalizes, an emptied message may disappear).
fn repair_tool_pairs(msgs: &mut Vec<OutMsg>) -> bool {
    let synthetic = |id: &str| {
        json!({
            "type": "tool_result",
            "tool_use_id": id,
            "content": ORPHAN_RESULT_TEXT,
            "is_error": true,
        })
    };
    let mut dropped = false;
    let mut pending: Vec<String> = Vec::new();
    for m in msgs.iter_mut() {
        match m.role {
            Role::Assistant => pending = tool_use_ids(m),
            Role::User => {
                let before = m.blocks.len();
                let mut answered: HashSet<String> = HashSet::new();
                m.blocks.retain(|b| {
                    if block_type(b) != "tool_result" {
                        return true;
                    }
                    match str_of(b, "tool_use_id") {
                        Some(id) => {
                            pending.iter().any(|p| p == id) && answered.insert(id.to_string())
                        }
                        None => false,
                    }
                });
                dropped |= m.blocks.len() != before;
                for id in &pending {
                    if !answered.contains(id) {
                        m.blocks.push(synthetic(id));
                    }
                }
                pending.clear();
            }
        }
    }
    if !pending.is_empty() {
        let blocks = pending.iter().map(|id| synthetic(id)).collect();
        msgs.push(OutMsg {
            role: Role::User,
            blocks,
            timestamp: None,
        });
    }
    dropped
}

/// User `content`: tool results first (the API requires it), a lone text
/// block as a plain string.
fn user_content(blocks: &[Value]) -> Value {
    let mut ordered: Vec<Value> = blocks
        .iter()
        .filter(|b| block_type(b) == "tool_result")
        .cloned()
        .collect();
    ordered.extend(
        blocks
            .iter()
            .filter(|b| block_type(b) != "tool_result")
            .cloned(),
    );
    if ordered.len() == 1 && block_type(&ordered[0]) == "text" {
        if let Some(t) = str_of(&ordered[0], "text") {
            return Value::String(t.to_string());
        }
    }
    Value::Array(ordered)
}

/// A random (v4) UUID, lowercase hyphenated. The OS RNG failing is not worth
/// a panic: the fallback only needs to be unique within the file.
fn new_uuid_v4() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let digest = Sha256::digest(format!("{nanos}-{n}-{}", std::process::id()).as_bytes());
        b.copy_from_slice(&digest[..16]);
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// The JSONL contents of `<new_session_id>.jsonl` for `claude -r`: the
/// preamble as the first user message, then the plan's turns under the
/// hazards documented in the module doc, then `ai-title` and `last-prompt`
/// trailer lines (the resume picker's title, and the leaf CC resumes from).
///
/// Tool calls follow [`plan_tool_call`] for Claude: native ones stay
/// `tool_use`/`tool_result` pairs under a [`safe_tool_id`]; flattened ones
/// become text in both directions.
pub fn write_claude_transcript(plan: &TransplantPlan, claude_version: &str) -> String {
    let mut msgs: Vec<OutMsg> = vec![OutMsg {
        role: Role::User,
        blocks: vec![text_block(&plan.preamble)],
        timestamp: None,
    }];

    let mut calls: HashMap<&str, CallPlan> = HashMap::new();
    for turn in &plan.turns {
        let mut blocks: Vec<Value> = Vec::new();
        for b in &turn.blocks {
            match (turn.role, b) {
                (_, Block::Text { text }) => blocks.push(text_block(text)),
                (Role::User, Block::Image { media_type, data }) => blocks.push(json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data },
                })),
                (Role::Assistant, Block::ToolCall { id, name, input }) => {
                    match plan_tool_call(Harness::Claude, name, input) {
                        ToolPlan::Native {
                            name: native_name,
                            input: native_input,
                        } => {
                            let safe_id = safe_tool_id(id);
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": safe_id,
                                "name": native_name,
                                "input": native_input,
                            }));
                            calls.insert(id.as_str(), CallPlan::Native { safe_id });
                        }
                        ToolPlan::Flatten => {
                            blocks.push(text_block(&flattened_call_text(
                                name,
                                input,
                                FLATTEN_CALL_MAX_CHARS,
                            )));
                            calls.insert(id.as_str(), CallPlan::Flatten { name: name.clone() });
                        }
                    }
                }
                (
                    Role::User,
                    Block::ToolResult {
                        call_id,
                        name,
                        text,
                        is_error,
                    },
                ) => match calls.get(call_id.as_str()) {
                    Some(CallPlan::Native { safe_id }) => blocks.push(json!({
                        "type": "tool_result",
                        "tool_use_id": safe_id,
                        "content": if text.is_empty() { EMPTY_RESULT_TEXT } else { text.as_str() },
                        "is_error": is_error,
                    })),
                    Some(CallPlan::Flatten { name: call_name }) => {
                        let label = name.as_deref().unwrap_or(call_name.as_str());
                        blocks.push(text_block(&flattened_result_text(
                            Some(label),
                            text,
                            *is_error,
                            usize::MAX,
                        )));
                    }
                    // Orphan result: no call in the plan answers it.
                    None => {}
                },
                // Thinking is never written; images only exist on user turns;
                // a result on an assistant turn or a call on a user turn is
                // malformed input and carries nothing a transcript can hold.
                _ => {}
            }
        }
        msgs.push(OutMsg {
            role: turn.role,
            blocks,
            timestamp: iso_timestamp(turn.timestamp.as_deref()),
        });
    }

    loop {
        normalize(&mut msgs);
        if !repair_tool_pairs(&mut msgs) {
            break;
        }
    }

    let session_id = plan.new_session_id.as_str();
    let mut lines: Vec<String> = Vec::with_capacity(msgs.len() + 2);
    let mut parent: Option<String> = None;
    let mut assistant_count = 0usize;
    for m in &msgs {
        let uuid = new_uuid_v4();
        let timestamp = m.timestamp.as_deref().unwrap_or(&plan.created_at);
        let (ty, message) = match m.role {
            Role::User => (
                "user",
                json!({ "role": "user", "content": user_content(&m.blocks) }),
            ),
            Role::Assistant => {
                assistant_count += 1;
                let stop_reason = if m.blocks.iter().any(|b| block_type(b) == "tool_use") {
                    "tool_use"
                } else {
                    "end_turn"
                };
                let mut msg = Map::new();
                msg.insert(
                    "id".into(),
                    Value::String(format!("msg_kbhandoff{assistant_count:08}")),
                );
                msg.insert("type".into(), json!("message"));
                msg.insert("role".into(), json!("assistant"));
                if let Some(model) = &plan.target_model {
                    msg.insert("model".into(), json!(model));
                }
                msg.insert("content".into(), Value::Array(m.blocks.clone()));
                msg.insert("stop_reason".into(), json!(stop_reason));
                msg.insert("stop_sequence".into(), Value::Null);
                msg.insert(
                    "usage".into(),
                    json!({ "input_tokens": 0, "output_tokens": 0 }),
                );
                ("assistant", Value::Object(msg))
            }
        };
        let mut record = json!({
            "parentUuid": parent,
            "isSidechain": false,
            "userType": "external",
            "entrypoint": "cli",
            "cwd": plan.cwd,
            "sessionId": session_id,
            "version": claude_version,
            "type": ty,
            "uuid": uuid,
            "timestamp": timestamp,
            "message": message,
        });
        // The preamble record carries the `[kb-handoff …]` lineage marker and
        // kb's first-prompt ladder prefers `typed` records, so it must be one
        // or the first prompt the operator types would displace it.
        if parent.is_none() {
            record["promptSource"] = json!("typed");
        }
        lines.push(record.to_string());
        parent = Some(uuid);
    }

    let title = plan.title.clone().unwrap_or_else(|| {
        let short: String = plan.source_session_id.chars().take(8).collect();
        format!("Handoff from {} {short}", plan.source.as_str())
    });
    lines
        .push(json!({ "type": "ai-title", "aiTitle": title, "sessionId": session_id }).to_string());
    if let Some(leaf) = parent {
        let last_prompt: String = plan.preamble.chars().take(LAST_PROMPT_CHARS).collect();
        lines.push(
            json!({
                "type": "last-prompt",
                "lastPrompt": last_prompt,
                "leafUuid": leaf,
                "sessionId": session_id,
            })
            .to_string(),
        );
    }

    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn jsonl(records: &[Value]) -> String {
        records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn text_of(turn: &Turn) -> String {
        turn.text()
    }

    #[test]
    fn raw_transcript_walks_chain_across_compaction() {
        let sid = "11111111-2222-4333-8444-555555555555";
        let rec = |ty: &str, uuid: &str, parent: Value, message: Value, ts: &str| {
            json!({
                "type": ty, "uuid": uuid, "parentUuid": parent, "sessionId": sid,
                "cwd": "/work/app", "gitBranch": "main", "isSidechain": false,
                "timestamp": ts, "message": message,
            })
        };
        let lines = vec![
            json!({"type": "mode", "mode": "default", "sessionId": sid}),
            rec(
                "user",
                "u1",
                Value::Null,
                json!({"role": "user", "content": "first prompt"}),
                "2026-11-01T10:00:00.000Z",
            ),
            rec(
                "assistant",
                "a1",
                json!("u1"),
                json!({"id": "msg_1", "role": "assistant", "model": "claude-opus-5-5",
                       "content": [{"type": "thinking", "thinking": "hmm", "signature": "sig"}]}),
                "2026-11-01T10:00:01.000Z",
            ),
            rec(
                "assistant",
                "a2",
                json!("a1"),
                json!({"id": "msg_1", "role": "assistant", "model": "claude-opus-5-5",
                       "content": [{"type": "tool_use", "id": "toolu_1", "name": "Bash",
                                    "input": {"command": "ls"}}]}),
                "2026-11-01T10:00:02.000Z",
            ),
            rec(
                "user",
                "u2",
                json!("a2"),
                json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "file.txt"}]}),
                "2026-11-01T10:00:03.000Z",
            ),
            json!({"type": "attachment", "uuid": "att1", "parentUuid": "u2",
                   "sessionId": sid, "attachment": {"type": "hook_success"}}),
            json!({"type": "system", "subtype": "compact_boundary", "uuid": "b1",
                   "parentUuid": null, "logicalParentUuid": "att1", "sessionId": sid,
                   "content": "Conversation compacted"}),
            json!({"type": "user", "uuid": "s1", "parentUuid": "b1", "sessionId": sid,
                   "isCompactSummary": true, "timestamp": "2026-11-01T11:00:00.000Z",
                   "message": {"role": "user", "content": "This session is being continued.\n\nSummary: did things"}}),
            json!({"type": "user", "uuid": "m1", "parentUuid": "s1", "sessionId": sid,
                   "isMeta": true,
                   "message": {"role": "user", "content": "<local-command-caveat>x</local-command-caveat>"}}),
            rec(
                "user",
                "u3",
                json!("m1"),
                json!({"role": "user", "content": "continue"}),
                "2026-11-01T11:00:05.000Z",
            ),
            rec(
                "assistant",
                "a3",
                json!("u3"),
                json!({"id": "msg_2", "role": "assistant", "model": "claude-opus-5-5",
                       "content": [{"type": "text", "text": "done"}]}),
                "2026-11-01T11:00:06.000Z",
            ),
            json!({"type": "assistant", "uuid": "sc1", "parentUuid": "a3", "sessionId": sid,
                   "isSidechain": true,
                   "message": {"id": "msg_sc", "role": "assistant", "model": "claude-haiku-5-5",
                               "content": [{"type": "text", "text": "subagent chatter"}]}}),
            json!({"type": "ai-title", "aiTitle": "AI title", "sessionId": sid}),
            json!({"type": "custom-title", "customTitle": "My title", "sessionId": sid}),
            json!({"type": "last-prompt", "lastPrompt": "continue", "leafUuid": "a3",
                   "sessionId": sid}),
        ];

        let check = |lines: &[Value]| {
            let conv =
                read_claude_jsonl(&jsonl(lines), SourceKind::Raw, Some("/x/s.jsonl")).unwrap();
            assert_eq!(conv.harness, Harness::Claude);
            assert_eq!(conv.session_id, sid);
            assert_eq!(conv.source_path.as_deref(), Some("/x/s.jsonl"));
            assert_eq!(conv.cwd.as_deref(), Some("/work/app"));
            assert_eq!(conv.git_branch.as_deref(), Some("main"));
            assert_eq!(conv.title.as_deref(), Some("My title"));
            assert_eq!(conv.model.as_deref(), Some("claude-opus-5-5"));
            assert_eq!(conv.started_at.as_deref(), Some("2026-11-01T10:00:00.000Z"));
            assert_eq!(conv.ended_at.as_deref(), Some("2026-11-01T11:00:06.000Z"));

            assert_eq!(conv.turns.len(), 5, "{:#?}", conv.turns);
            assert_eq!(text_of(&conv.turns[0]), "first prompt");
            // thinking + tool_use lines of one message.id merged, in order.
            assert_eq!(conv.turns[1].role, Role::Assistant);
            assert_eq!(
                conv.turns[1].blocks,
                vec![
                    Block::Thinking {
                        text: "hmm".to_string()
                    },
                    Block::ToolCall {
                        id: "toolu_1".to_string(),
                        name: "Bash".to_string(),
                        input: json!({"command": "ls"}),
                    },
                ]
            );
            assert_eq!(
                conv.turns[2].blocks,
                vec![Block::ToolResult {
                    call_id: "toolu_1".to_string(),
                    name: Some("Bash".to_string()),
                    text: "file.txt".to_string(),
                    is_error: false,
                }]
            );
            // isMeta, attachment, system and the sidechain line are not turns;
            // the compact summary is the summary, not a turn.
            assert_eq!(text_of(&conv.turns[3]), "continue");
            assert_eq!(text_of(&conv.turns[4]), "done");
            let summary = conv.summary.as_ref().unwrap();
            assert_eq!(summary.origin, SummaryOrigin::ClaudeCompact);
            assert_eq!(summary.covers, 3);
            assert_eq!(summary.at.as_deref(), Some("2026-11-01T11:00:00.000Z"));
            assert!(summary.text.contains("Summary: did things"));
        };
        check(&lines);

        // Without last-prompt the leaf is the last main-thread record.
        let without: Vec<Value> = lines
            .iter()
            .filter(|l| type_of(l) != "last-prompt")
            .cloned()
            .collect();
        check(&without);
    }

    #[test]
    fn omp_capture_reads_in_file_order() {
        let sid = "01a1e4a1-0d74-7548-9455-e37ffe71b0a1";
        let lines = vec![
            json!({"sessionId": sid, "type": "adapter-meta", "adapter": "kb-capture-omp/1",
                   "harness": "omp", "cwd": "/work/omp", "title": "slot title",
                   "session_file": "/home/u/.omp/agent/sessions/x/2026_s.jsonl",
                   "timestamp": "unknown", "aiTitle": "Ai title"}),
            json!({"sessionId": sid, "timestamp": "2026-11-02T09:00:00.000Z", "cwd": "/work/omp",
                   "type": "user",
                   "message": {"role": "user", "content": [{"type": "text", "text": "list files"}]}}),
            json!({"sessionId": sid, "timestamp": "2026-11-02T09:00:01.000Z", "cwd": "/work/omp",
                   "type": "assistant",
                   "message": {"role": "assistant", "model": "anthropic/claude-opus-5-5",
                               "content": [{"type": "text", "text": "running"},
                                           {"type": "tool_use", "id": "call_1|fc_1", "name": "Bash",
                                            "input": {"command": "ls"}}]}}),
            json!({"sessionId": sid, "timestamp": "2026-11-02T09:00:02.000Z", "cwd": "/work/omp",
                   "type": "user",
                   "message": {"role": "user", "content": [
                       {"type": "tool_result", "tool_use_id": "call_1|fc_1", "is_error": false,
                        "content": [{"type": "text", "text": "[intent] list\na.txt"}]}]}}),
            json!({"type": "attachment", "uuid": "att", "parentUuid": null, "sessionId": sid,
                   "attachment": {"type": "hook_additional_context"}}),
            json!({"sessionId": sid, "timestamp": "2026-11-02T09:05:00.000Z", "cwd": "/work/omp",
                   "type": "assistant",
                   "message": {"role": "assistant",
                               "content": [{"type": "text", "text": "[compaction] Headline\n\nBody of summary"}]}}),
            json!({"sessionId": sid, "timestamp": "unknown", "cwd": "/work/omp",
                   "type": "assistant",
                   "message": {"role": "assistant",
                               "content": [{"type": "text", "text": "[session-exit] aborted"}]}}),
            json!({"sessionId": sid, "timestamp": "2026-11-02T09:06:00.000Z", "cwd": "/work/omp",
                   "type": "user",
                   "message": {"role": "user", "content": [{"type": "text", "text": "next step"}]}}),
        ];
        let conv =
            read_claude_jsonl(&jsonl(&lines), SourceKind::Capture, Some("/c/cap.html")).unwrap();
        assert_eq!(conv.harness, Harness::Omp);
        assert_eq!(conv.session_id, sid);
        assert_eq!(conv.title.as_deref(), Some("Ai title"));
        assert_eq!(conv.cwd.as_deref(), Some("/work/omp"));
        assert_eq!(conv.turns.len(), 4, "{:#?}", conv.turns);
        assert_eq!(text_of(&conv.turns[0]), "list files");
        assert_eq!(conv.turns[1].role, Role::Assistant);
        assert_eq!(
            conv.turns[1].model.as_deref(),
            Some("anthropic/claude-opus-5-5")
        );
        assert_eq!(
            conv.turns[2].blocks,
            vec![Block::ToolResult {
                call_id: "call_1|fc_1".to_string(),
                name: Some("Bash".to_string()),
                text: "[intent] list\na.txt".to_string(),
                is_error: false,
            }]
        );
        assert_eq!(text_of(&conv.turns[3]), "next step");
        let summary = conv.summary.as_ref().unwrap();
        assert_eq!(summary.origin, SummaryOrigin::CaptureCompaction);
        assert_eq!(summary.text, "Headline\n\nBody of summary");
        assert_eq!(summary.covers, 3);
        assert!(conv
            .notes
            .iter()
            .any(|n| n == "raw file: /home/u/.omp/agent/sessions/x/2026_s.jsonl"));
        assert!(conv.notes.iter().any(|n| n.contains("lossy")));
        assert_eq!(conv.ended_at.as_deref(), Some("2026-11-02T09:06:00.000Z"));
    }

    #[test]
    fn foreign_capture_and_empty_transcripts_are_errors() {
        let codex = jsonl(&[
            json!({"sessionId": "s1", "type": "adapter-meta", "harness": "codex"}),
            json!({"sessionId": "s1", "type": "user",
                   "message": {"role": "user", "content": "hi"}}),
        ]);
        assert!(matches!(
            read_claude_jsonl(&codex, SourceKind::Capture, None),
            Err(HandoffError::NotASession { .. })
        ));
        let empty = jsonl(&[json!({"sessionId": "s2", "type": "mode", "mode": "default"})]);
        assert!(matches!(
            read_claude_jsonl(&empty, SourceKind::Raw, None),
            Err(HandoffError::Empty { .. })
        ));
        assert!(matches!(
            read_claude_jsonl("not json at all", SourceKind::Raw, None),
            Err(HandoffError::NotASession { .. })
        ));
    }

    fn plan_fixture() -> TransplantPlan {
        let ts = |s: &str| Some(s.to_string());
        let turn = |role, timestamp: Option<String>, blocks| Turn {
            role,
            timestamp,
            model: None,
            blocks,
        };
        TransplantPlan {
            source: Harness::Omp,
            source_session_id: "01a1e4a1-0d74-7548-9455-e37ffe71b0a1".to_string(),
            target: Harness::Claude,
            new_session_id: "99999999-8888-4777-8666-555555555555".to_string(),
            cwd: "/work/app".to_string(),
            title: None,
            created_at: "2026-11-16T12:00:00.000Z".to_string(),
            preamble:
                "[kb-handoff from omp session 01a1e4a1-0d74-7548-9455-e37ffe71b0a1]\n\nSummary."
                    .to_string(),
            turns: vec![
                turn(
                    Role::Assistant,
                    ts("2026-11-16T10:00:00.000Z"),
                    vec![
                        Block::Thinking {
                            text: "private reasoning".to_string(),
                        },
                        Block::Text {
                            text: "  ".to_string(),
                        },
                        Block::Text {
                            text: "running it".to_string(),
                        },
                        Block::ToolCall {
                            id: "call_x|fc_y".to_string(),
                            name: "bash".to_string(),
                            input: json!({"command": "ls"}),
                        },
                    ],
                ),
                turn(
                    Role::User,
                    ts("2026-11-16T10:00:01.000Z"),
                    vec![Block::ToolResult {
                        call_id: "call_x|fc_y".to_string(),
                        name: Some("bash".to_string()),
                        text: "a.txt".to_string(),
                        is_error: false,
                    }],
                ),
                turn(
                    Role::Assistant,
                    ts("2026-11-16T10:00:02.000Z"),
                    vec![Block::ToolCall {
                        id: "call_e".to_string(),
                        name: "edit".to_string(),
                        input: json!({"path": "src/a.rs", "edits": []}),
                    }],
                ),
                turn(
                    Role::User,
                    None,
                    vec![Block::ToolResult {
                        call_id: "call_e".to_string(),
                        name: Some("edit".to_string()),
                        text: "applied".to_string(),
                        is_error: false,
                    }],
                ),
                turn(
                    Role::Assistant,
                    ts("2026-11-16T10:00:04.000Z"),
                    vec![Block::ToolCall {
                        id: "call_orphan".to_string(),
                        name: "bash".to_string(),
                        input: json!({"command": "sleep 100"}),
                    }],
                ),
                turn(
                    Role::Assistant,
                    ts("2026-11-16T10:00:05.000Z"),
                    vec![
                        Block::Thinking {
                            text: "more".to_string(),
                        },
                        Block::Text {
                            text: "after the call".to_string(),
                        },
                    ],
                ),
                turn(
                    Role::User,
                    ts("not a timestamp"),
                    vec![
                        Block::ToolResult {
                            call_id: "ghost".to_string(),
                            name: None,
                            text: "nobody asked".to_string(),
                            is_error: false,
                        },
                        Block::Text {
                            text: "thanks".to_string(),
                        },
                    ],
                ),
            ],
            omitted_turns: 12,
            target_model: Some("claude-opus-5-5".to_string()),
        }
    }

    #[test]
    fn writer_emits_a_resumable_chain() {
        let plan = plan_fixture();
        let out = write_claude_transcript(&plan, "2.1.318");
        assert!(out.ends_with('\n'));
        assert!(!out.contains("\"thinking\""), "no thinking blocks");
        assert!(!out.contains("private reasoning"));
        assert!(!out.contains("gitBranch"));
        let lines: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).expect("every line is JSON"))
            .collect();
        let conv: Vec<&Value> = lines.iter().filter(|r| is_conversation_type(r)).collect();

        // preamble, A(text+Bash), U(result), A(flattened edit), U(flat result),
        // A(orphan Bash + text), U(synthetic result + thanks)
        assert_eq!(conv.len(), 7, "{out}");
        let mut prev_uuid: Option<&str> = None;
        let mut prev_role = "";
        for r in &conv {
            assert_eq!(r["parentUuid"].as_str(), prev_uuid);
            let ts = r["timestamp"].as_str().unwrap();
            assert!(DateTime::parse_from_rfc3339(ts).is_ok(), "bad ts {ts}");
            assert_eq!(r["sessionId"], plan.new_session_id);
            assert_eq!(r["cwd"], "/work/app");
            assert_eq!(r["version"], "2.1.318");
            assert_eq!(r["isSidechain"], false);
            assert_eq!(r["userType"], "external");
            assert_eq!(r["entrypoint"], "cli");
            assert_ne!(type_of(r), prev_role, "roles must alternate");
            prev_role = type_of(r);
            prev_uuid = r["uuid"].as_str();
        }
        let uuids: HashSet<&str> = conv.iter().filter_map(|r| r["uuid"].as_str()).collect();
        assert_eq!(uuids.len(), conv.len(), "uuids are unique");
        assert!(conv[0]["parentUuid"].is_null());
        assert_eq!(conv[0]["message"]["content"], plan.preamble);
        assert_eq!(conv[0]["timestamp"], plan.created_at);
        // An unparsable turn timestamp falls back to created_at.
        assert_eq!(conv[6]["timestamp"], plan.created_at);

        // Native Bash pair: composite id rewritten identically on both sides.
        let safe = safe_tool_id("call_x|fc_y");
        assert!(safe.starts_with("toolu_"));
        let a1 = &conv[1]["message"];
        assert_eq!(a1["id"], "msg_kbhandoff00000001");
        assert_eq!(a1["model"], "claude-opus-5-5");
        assert_eq!(a1["stop_reason"], "tool_use");
        assert_eq!(
            a1["content"],
            json!([
                {"type": "text", "text": "running it"},
                {"type": "tool_use", "id": safe, "name": "Bash", "input": {"command": "ls"}},
            ])
        );
        assert_eq!(
            conv[2]["message"]["content"],
            json!([{"type": "tool_result", "tool_use_id": safe, "content": "a.txt",
                    "is_error": false}])
        );

        // Flattened edit pair: plain text both ways.
        let flat_call = conv[3]["message"]["content"][0]["text"].as_str().unwrap();
        assert!(flat_call.starts_with("[tool call · edit"), "{flat_call}");
        assert_eq!(conv[3]["message"]["stop_reason"], "end_turn");
        assert_eq!(
            conv[4]["message"]["content"],
            "[tool result · edit]\napplied"
        );

        // Adjacent assistant turns merged; orphan call got a synthetic error
        // result in the next user record, the ghost result was dropped.
        let a3 = &conv[5]["message"];
        assert_eq!(a3["id"], "msg_kbhandoff00000003");
        let orphan_id = safe_tool_id("call_orphan");
        assert_eq!(a3["content"][0]["type"], "tool_use");
        assert_eq!(a3["content"][0]["id"], orphan_id);
        assert_eq!(
            a3["content"][1],
            json!({"type": "text", "text": "after the call"})
        );
        let u3 = conv[6]["message"]["content"].as_array().unwrap();
        assert_eq!(u3.len(), 2);
        assert_eq!(u3[0]["type"], "tool_result");
        assert_eq!(u3[0]["tool_use_id"], orphan_id);
        assert_eq!(u3[0]["is_error"], true);
        assert!(u3[0]["content"]
            .as_str()
            .unwrap()
            .contains("no result recorded"));
        assert_eq!(u3[1], json!({"type": "text", "text": "thanks"}));
        assert!(!out.contains("nobody asked"));

        // Trailer: title + the leaf CC resumes from.
        let n = lines.len();
        assert_eq!(lines[n - 2]["type"], "ai-title");
        assert_eq!(lines[n - 2]["aiTitle"], "Handoff from omp 01a1e4a1");
        assert_eq!(lines[n - 2]["sessionId"], plan.new_session_id);
        assert_eq!(lines[n - 1]["type"], "last-prompt");
        assert_eq!(lines[n - 1]["leafUuid"], conv[6]["uuid"]);
        assert_eq!(lines[n - 1]["sessionId"], plan.new_session_id);
        assert!(lines[n - 1]["lastPrompt"]
            .as_str()
            .unwrap()
            .starts_with("[kb-handoff from omp session"));
    }

    #[test]
    fn writer_output_reads_back_through_the_reader() {
        let plan = plan_fixture();
        let out = write_claude_transcript(&plan, "2.1.318");
        let conv = read_claude_jsonl(&out, SourceKind::Raw, None).unwrap();
        assert_eq!(conv.harness, Harness::Claude);
        assert_eq!(conv.session_id, plan.new_session_id);
        assert_eq!(conv.turns.len(), 7);
        assert_eq!(conv.title.as_deref(), Some("Handoff from omp 01a1e4a1"));
        assert_eq!(conv.model.as_deref(), Some("claude-opus-5-5"));
        assert!(conv.turns[0]
            .text()
            .starts_with("[kb-handoff from omp session"));
    }

    fn plan_with_turns(turns: Vec<Turn>) -> TransplantPlan {
        TransplantPlan {
            turns,
            ..plan_fixture()
        }
    }

    fn written_records(plan: &TransplantPlan) -> Vec<Value> {
        write_claude_transcript(plan, "2.1.318")
            .lines()
            .map(|l| serde_json::from_str(l).expect("every line is JSON"))
            .collect()
    }

    #[test]
    fn only_the_first_user_record_is_marked_typed() {
        let plan = plan_fixture();
        let lines = written_records(&plan);
        assert_eq!(type_of(&lines[0]), "user");
        assert_eq!(lines[0]["promptSource"], "typed");
        assert_eq!(
            lines
                .iter()
                .filter(|r| r.get("promptSource").is_some())
                .count(),
            1,
            "no other record carries promptSource"
        );

        // A first plan prompt merged into the preamble record keeps the single marker.
        let merged = plan_with_turns(vec![
            Turn {
                role: Role::User,
                timestamp: Some("2026-11-16T10:00:00.000Z".to_string()),
                model: None,
                blocks: vec![Block::Text {
                    text: "first prompt".to_string(),
                }],
            },
            Turn {
                role: Role::Assistant,
                timestamp: Some("2026-11-16T10:00:01.000Z".to_string()),
                model: None,
                blocks: vec![Block::Text {
                    text: "answer".to_string(),
                }],
            },
        ]);
        let lines = written_records(&merged);
        let users: Vec<&Value> = lines.iter().filter(|r| type_of(r) == "user").collect();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0]["promptSource"], "typed");
        assert_eq!(
            lines
                .iter()
                .filter(|r| r.get("promptSource").is_some())
                .count(),
            1
        );
    }

    #[test]
    fn planner_clipped_flattened_result_keeps_one_true_marker() {
        let clipped = super::super::tools::clip_middle(&"x".repeat(50_000), 20_000);
        assert!(clipped.contains("[… 30000 chars omitted …]"));
        let plan = plan_with_turns(vec![
            Turn {
                role: Role::Assistant,
                timestamp: Some("2026-11-16T10:00:00.000Z".to_string()),
                model: None,
                blocks: vec![Block::ToolCall {
                    id: "call_e".to_string(),
                    name: "edit".to_string(),
                    input: json!({"path": "src/a.rs", "edits": []}),
                }],
            },
            Turn {
                role: Role::User,
                timestamp: Some("2026-11-16T10:00:01.000Z".to_string()),
                model: None,
                blocks: vec![Block::ToolResult {
                    call_id: "call_e".to_string(),
                    name: Some("edit".to_string()),
                    text: clipped.clone(),
                    is_error: false,
                }],
            },
        ]);
        let lines = written_records(&plan);
        let result = lines
            .iter()
            .filter(|r| type_of(r) == "user")
            .find_map(|r| {
                let content = &r["message"]["content"];
                let texts: Vec<&str> = match content {
                    Value::String(s) => vec![s.as_str()],
                    Value::Array(blocks) => blocks
                        .iter()
                        .filter(|b| b["type"] == "text")
                        .filter_map(|b| b["text"].as_str())
                        .collect(),
                    _ => Vec::new(),
                };
                texts
                    .into_iter()
                    .find(|t| t.starts_with("[tool result · edit]"))
            })
            .expect("flattened result record");
        assert_eq!(result.matches("chars omitted").count(), 1, "{result:.200}");
        assert!(result.contains("[… 30000 chars omitted …]"));
        assert!(result.ends_with(&clipped[clipped.len() - 100..]));
    }

    #[test]
    fn empty_native_result_is_written_as_no_output() {
        let plan = plan_with_turns(vec![
            Turn {
                role: Role::Assistant,
                timestamp: Some("2026-11-16T10:00:00.000Z".to_string()),
                model: None,
                blocks: vec![Block::ToolCall {
                    id: "call_b".to_string(),
                    name: "bash".to_string(),
                    input: json!({"command": "true"}),
                }],
            },
            Turn {
                role: Role::User,
                timestamp: Some("2026-11-16T10:00:01.000Z".to_string()),
                model: None,
                blocks: vec![Block::ToolResult {
                    call_id: "call_b".to_string(),
                    name: Some("bash".to_string()),
                    text: String::new(),
                    is_error: false,
                }],
            },
        ]);
        let lines = written_records(&plan);
        let result = lines
            .iter()
            .filter(|r| type_of(r) == "user")
            .filter_map(|r| r["message"]["content"].as_array())
            .flatten()
            .find(|b| b["type"] == "tool_result")
            .expect("native tool_result");
        assert_eq!(result["content"], "(no output)");
    }

    #[test]
    fn session_id_that_is_not_a_plain_token_is_rejected() {
        for bad in ["a`b\nc", "a b", "-lead", ""] {
            let raw = jsonl(&[json!({"sessionId": bad, "type": "user",
                       "uuid": "u1", "message": {"role": "user", "content": "hi"}})]);
            assert!(
                matches!(
                    read_claude_jsonl(&raw, SourceKind::Raw, None),
                    Err(HandoffError::NotASession { .. })
                ),
                "{bad:?}"
            );
        }
        let meta = jsonl(&[
            json!({"sessionId": "bad`id", "type": "adapter-meta", "harness": "claude"}),
            json!({"sessionId": "s1", "type": "user",
                   "message": {"role": "user", "content": "hi"}}),
        ]);
        assert!(matches!(
            read_claude_jsonl(&meta, SourceKind::Capture, None),
            Err(HandoffError::NotASession { .. })
        ));
    }

    #[test]
    fn trailing_orphan_call_gets_a_closing_user_record() {
        let mut plan = plan_fixture();
        plan.turns = vec![Turn {
            role: Role::Assistant,
            timestamp: None,
            model: None,
            blocks: vec![Block::ToolCall {
                id: "t1".to_string(),
                name: "bash".to_string(),
                input: json!({"command": "make"}),
            }],
        }];
        let out = write_claude_transcript(&plan, "2.1.318");
        let lines: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let conv: Vec<&Value> = lines.iter().filter(|r| is_conversation_type(r)).collect();
        assert_eq!(conv.len(), 3);
        assert_eq!(conv[2]["type"], "user");
        assert_eq!(conv[2]["message"]["content"][0]["tool_use_id"], "t1");
        assert_eq!(conv[2]["message"]["content"][0]["is_error"], true);
    }

    #[test]
    fn roots_and_locate_walk_the_claude_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let touch = |p: &Path, age_secs: u64| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "{}\n").unwrap();
            let f = fs::File::options().write(true).open(p).unwrap();
            f.set_modified(SystemTime::now() - std::time::Duration::from_secs(age_secs))
                .unwrap();
        };
        let default_root = home.join(".claude").join("projects");
        let account_root = home.join(".claude-accounts").join("work").join("projects");
        let config_dir = home.join("cfg");
        let config_root = config_dir.join("projects");
        let older = default_root
            .join("-work-app")
            .join("ABCD1234-0000-4000-8000-000000000001.jsonl");
        let newer = account_root
            .join("-work-other")
            .join("abcd1234-ffff-4000-8000-000000000002.jsonl");
        touch(&older, 600);
        touch(&newer, 10);
        // Not sessions: a subagent sidecar, a non-jsonl file, another id.
        touch(
            &default_root
                .join("-work-app")
                .join("abcd1234-0000-4000-8000-000000000001")
                .join("subagents")
                .join("abcd-agent.jsonl"),
            1,
        );
        touch(
            &default_root.join("-work-app").join("abcd1234-notes.txt"),
            1,
        );
        touch(
            &default_root
                .join("-work-app")
                .join("ffff0000-0000-4000-8000-000000000003.jsonl"),
            1,
        );
        fs::create_dir_all(&config_root).unwrap();

        let roots = claude_projects_roots(Some(&config_dir), home);
        assert_eq!(
            roots,
            vec![
                config_root.clone(),
                default_root.clone(),
                account_root.clone()
            ]
        );

        // A config dir that is ~/.claude itself is listed once.
        let roots = claude_projects_roots(Some(&home.join(".claude")), home);
        assert_eq!(roots, vec![default_root.clone(), account_root.clone()]);

        // Missing dirs are left out entirely.
        let bare = tempfile::tempdir().unwrap();
        assert!(claude_projects_roots(None, bare.path()).is_empty());

        let found = locate_claude_session(&roots, "AbCd1234").unwrap();
        assert_eq!(found, vec![newer.clone(), older.clone()]);
        let found = locate_claude_session(&roots, "abcd1234-0000").unwrap();
        assert_eq!(found, vec![older]);
        assert!(locate_claude_session(&roots, "zzzz").unwrap().is_empty());
        assert!(locate_claude_session(&roots, "").unwrap().is_empty());
        assert!(locate_claude_session(&[home.join("missing")], "abcd")
            .unwrap()
            .is_empty());
    }
}
