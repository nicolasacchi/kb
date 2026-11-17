//! The omp half of the handoff: locate and read an omp session file into the
//! neutral [`Conversation`], and write a [`TransplantPlan`] as a NEW omp
//! session file `omp -r <id-prefix>` resumes.
//!
//! Format facts (all probed against omp 18.20.3, see the research notes):
//!
//! * `~/.omp/agent/sessions/<bucket>/<ts>_<sessionId>.jsonl`; `omp -r` looks
//!   sessions up globally, so the bucket is only a hint, but we still use
//!   omp's own bucket rule ([`omp_bucket_name`]).
//! * Line 1 is an optional 256-byte `type:"title"` slot (the freshest title),
//!   then the `type:"session"` header, then tree entries chained by
//!   `id`/`parentId`. The leaf is the last physical entry (or the `activeLeafId`
//!   it names); the live chain is its ancestor path, cut at the last `reset_boundary` ([`live_chain_by`]).
//! * The loader is lenient: malformed lines are skipped, `usage`/`api` are
//!   optional on read, `blob:sha256:` image refs are only resolved by omp.
//!
//! Everything here is pure except [`locate_omp_session`], which lists
//! directories. The reader never resolves image blobs (no IO): they are
//! dropped with a note.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Map, Value};

use super::ir::{Block, Conversation, Harness, Role, SourceKind, Summary, SummaryOrigin, Turn};
use super::tools::{
    flattened_call_text, flattened_result_text, plan_tool_call, safe_tool_id, ToolPlan,
};
use super::transplant::TransplantPlan;
use super::HandoffError;

/// How much of a flattened call line the writer keeps.
const FLATTENED_CALL_CHARS: usize = 400;

/// Exact byte length of omp's title slot line, newline included.
const TITLE_SLOT_BYTES: usize = 256;

// ---------------------------------------------------------------------------
// Locating

/// `<home>/.omp/agent/sessions`.
pub fn omp_sessions_root(home: &Path) -> PathBuf {
    home.join(".omp").join("agent").join("sessions")
}

/// Top-level `*.jsonl` session files of every bucket under `sessions_root`
/// whose id part (the text after the last `_` of the file stem, or the whole
/// stem when there is none) starts with `id_prefix`, case-insensitively.
/// Newest first (mtime, then file name). Subagent transcripts live in
/// `<session>/` subdirectories and are not top-level, so they never match.
/// A missing root, or an empty prefix, yields `[]`.
pub fn locate_omp_session(sessions_root: &Path, id_prefix: &str) -> std::io::Result<Vec<PathBuf>> {
    let prefix = id_prefix.trim().to_ascii_lowercase();
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let buckets = match std::fs::read_dir(sessions_root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut hits: Vec<(SystemTime, PathBuf)> = Vec::new();
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
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") || !path.is_file() {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let id_part = stem.rsplit('_').next().unwrap_or(stem);
            if !id_part.to_ascii_lowercase().starts_with(&prefix) {
                continue;
            }
            let mtime = file
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            hits.push((mtime, path));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    Ok(hits.into_iter().map(|(_, p)| p).collect())
}

/// omp's own bucket-directory rule (`getDefaultSessionDirName`), applied to an
/// already canonical `cwd`:
///
/// * under `tmpdir` (checked first): `-tmp-<relative>` (`tmpdir` itself:
///   `-tmp`);
/// * under `home`: `-<relative>` (`home` itself: `-`);
/// * else `--<absolute path without the leading slash>--`;
///
/// with `/` (and, in the last form, `:`) replaced by `-`. Lossy by design —
/// the header `cwd` is the truth, the bucket is only a hint.
pub fn omp_bucket_name(cwd: &str, home: &str, tmpdir: &str) -> String {
    fn relative<'a>(path: &'a str, base: &str) -> Option<&'a str> {
        let base = base.trim_end_matches('/');
        if base.is_empty() {
            return None;
        }
        let path = path.trim_end_matches('/');
        if path == base {
            return Some("");
        }
        path.strip_prefix(base)?.strip_prefix('/')
    }
    if let Some(rel) = relative(cwd, tmpdir) {
        return if rel.is_empty() {
            "-tmp".to_string()
        } else {
            format!("-tmp-{}", rel.replace('/', "-"))
        };
    }
    if let Some(rel) = relative(cwd, home) {
        return format!("-{}", rel.replace('/', "-"));
    }
    let abs = cwd.trim_start_matches('/');
    format!("--{}--", abs.replace(['/', ':'], "-"))
}

// ---------------------------------------------------------------------------
// Leaf chain

/// The live leaf chain of an omp entry list, as indices into `entries` in
/// ascending file order. The walk is exactly the one kb's capture translator
/// and `kb sessions segment-plan` use:
///
/// * title entries are ignored;
/// * the walk starts at the LAST remaining entry and goes backward over file
///   order; an entry joins the chain when its id equals the cursor (the FIRST
///   backward match wins), and the cursor then becomes that entry's parent;
///   a missing (`None`) cursor matches nothing;
/// * the LAST reset entry on the chain cuts everything at or before it.
pub fn live_chain_by<T>(
    entries: &[T],
    id: impl Fn(&T) -> Option<&str>,
    parent: impl Fn(&T) -> Option<&str>,
    is_title: impl Fn(&T) -> bool,
    is_reset: impl Fn(&T) -> bool,
) -> Vec<usize> {
    let Some(last) = (0..entries.len()).rev().find(|&i| !is_title(&entries[i])) else {
        return Vec::new();
    };
    live_chain_from(entries, last, id, parent, is_title, is_reset)
}

/// [`live_chain_by`] with an explicit walk start: `start` indexes `entries`
/// and names the leaf. Only non-title entries at or before `start` take part
/// (so a repeated id later in the file cannot hijack the walk). The reader
/// uses it to start from omp's `activeLeafId`; [`live_chain_by`] starts from
/// the last non-title entry.
pub fn live_chain_from<T>(
    entries: &[T],
    start: usize,
    id: impl Fn(&T) -> Option<&str>,
    parent: impl Fn(&T) -> Option<&str>,
    is_title: impl Fn(&T) -> bool,
    is_reset: impl Fn(&T) -> bool,
) -> Vec<usize> {
    if start >= entries.len() {
        return Vec::new();
    }
    let mut cur: Option<&str> = id(&entries[start]);
    let mut chain: Vec<usize> = Vec::new();
    for i in (0..=start).rev() {
        let e = &entries[i];
        if is_title(e) {
            continue;
        }
        if cur.is_some() && id(e) == cur {
            chain.push(i);
            cur = parent(e);
        }
    }
    chain.reverse();
    let rb = chain
        .iter()
        .rposition(|&i| is_reset(&entries[i]))
        .map_or(0, |p| p + 1);
    chain.split_off(rb)
}

// ---------------------------------------------------------------------------
// Reading

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn iso_from_ms(ms: i64) -> Option<String> {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// An entry's ISO time: its own `timestamp`, else the message's ms epoch.
fn entry_timestamp(entry: &Value, msg: Option<&Value>) -> Option<String> {
    if let Some(s) = str_of(entry, "timestamp").filter(|s| !s.is_empty()) {
        return Some(s.to_string());
    }
    let ts = msg?.get("timestamp")?;
    let ms = ts.as_i64().or_else(|| ts.as_f64().map(|f| f as i64))?;
    iso_from_ms(ms)
}

fn user_blocks(content: Option<&Value>, blobs: &mut usize) -> Vec<Block> {
    let mut out = Vec::new();
    match content {
        Some(Value::String(s)) => {
            if !s.trim().is_empty() {
                out.push(Block::Text { text: s.clone() });
            }
        }
        Some(Value::Array(parts)) => {
            for p in parts {
                match str_of(p, "type") {
                    Some("text") => {
                        if let Some(t) = str_of(p, "text").filter(|t| !t.trim().is_empty()) {
                            out.push(Block::Text {
                                text: t.to_string(),
                            });
                        }
                    }
                    Some("image") => {
                        let Some(data) = str_of(p, "data").filter(|d| !d.is_empty()) else {
                            continue;
                        };
                        if data.starts_with("blob:") {
                            *blobs += 1;
                            continue;
                        }
                        let data = match data.split_once(";base64,") {
                            Some((head, rest)) if head.starts_with("data:") => rest,
                            _ => data,
                        };
                        out.push(Block::Image {
                            media_type: str_of(p, "mimeType").unwrap_or("image/png").to_string(),
                            data: data.to_string(),
                        });
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// A tool call's `arguments` as a JSON object (see [`Block::ToolCall`]).
fn call_input(arguments: Option<&Value>) -> Value {
    match arguments {
        Some(v @ Value::Object(_)) => v.clone(),
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(v @ Value::Object(_)) => v,
            _ => json!({ "_raw": s }),
        },
        None | Some(Value::Null) => json!({}),
        Some(other) => json!({ "_raw": other.to_string() }),
    }
}

fn assistant_blocks(content: Option<&Value>) -> Vec<Block> {
    let Some(Value::Array(parts)) = content else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for p in parts {
        match str_of(p, "type") {
            Some("text") => {
                if let Some(t) = str_of(p, "text").filter(|t| !t.trim().is_empty()) {
                    out.push(Block::Text {
                        text: t.to_string(),
                    });
                }
            }
            Some("thinking") => {
                if let Some(t) = str_of(p, "thinking").filter(|t| !t.trim().is_empty()) {
                    out.push(Block::Thinking {
                        text: t.to_string(),
                    });
                }
            }
            Some("toolCall") => {
                out.push(Block::ToolCall {
                    id: str_of(p, "id").unwrap_or_default().to_string(),
                    name: str_of(p, "name").unwrap_or_default().to_string(),
                    input: call_input(p.get("arguments")),
                });
            }
            // redactedThinking, anthropicServerTool, fallback, image: not portable.
            _ => {}
        }
    }
    out
}

fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|p| str_of(p, "type") == Some("text"))
            .filter_map(|p| str_of(p, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn assistant_model(msg: &Value) -> Option<String> {
    let model = str_of(msg, "model").filter(|m| !m.is_empty())?;
    match str_of(msg, "provider").filter(|p| !p.is_empty()) {
        Some(provider) => Some(format!("{provider}/{model}")),
        None => Some(model.to_string()),
    }
}

/// Read an omp session file (UTF-8 JSONL text) into a [`Conversation`].
///
/// The live chain is the leaf's ancestor path ([`live_chain_by`]); a v1 file
/// without any `id` is read in file order. Only `user` / `assistant` /
/// `toolResult` messages become turns (consecutive tool results fold into one
/// user turn); everything else (custom entries, model/thinking changes,
/// developer messages, executions …) is skipped. The newest `compaction`
/// entry on the chain becomes the [`Summary`].
pub fn read_omp_session(
    jsonl: &str,
    source_path: Option<&str>,
) -> Result<Conversation, HandoffError> {
    let not_a_session = |reason: &str| HandoffError::NotASession {
        harness: "omp",
        reason: reason.to_string(),
    };

    let mut malformed = 0usize;
    let mut slot_title: Option<String> = None;
    let mut header: Option<Value> = None;
    let mut entries: Vec<Value> = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str::<Value>(line) {
            Ok(v) if v.is_object() => v,
            _ => {
                malformed += 1;
                continue;
            }
        };
        let ty = str_of(&v, "type");
        if ty == Some("title") {
            if slot_title.is_none() {
                slot_title = str_of(&v, "title")
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string);
            }
            continue;
        }
        if header.is_none() {
            if ty == Some("session") && str_of(&v, "id").is_some() {
                header = Some(v);
                continue;
            }
            return Err(not_a_session(&format!(
                "first record is `{}`, not a session header with an id",
                ty.unwrap_or("untyped")
            )));
        }
        if ty == Some("session") {
            continue;
        }
        entries.push(v);
    }
    let Some(header) = header else {
        return Err(not_a_session("no session header"));
    };
    let session_id = str_of(&header, "id").unwrap_or_default().to_string();
    if !crate::sessions::resume::is_plain_token(&session_id) {
        return Err(not_a_session("session id is not a plain token"));
    }

    let is_reset = |e: &Value| str_of(e, "type") == Some("reset_boundary");
    let chain: Vec<usize> = if entries.iter().all(|e| e.get("id").is_none()) {
        // v1 file: no tree, file order is the conversation.
        let start = entries.iter().rposition(is_reset).map_or(0, |p| p + 1);
        (start..entries.len()).collect()
    } else {
        // omp marks late off-branch entries (`model_usage` …) with
        // `activeLeafId`: the real leaf, whose id must exist in the file.
        let start = entries
            .last()
            .and_then(|e| str_of(e, "activeLeafId"))
            .and_then(|leaf| entries.iter().rposition(|e| str_of(e, "id") == Some(leaf)))
            .unwrap_or(entries.len() - 1);
        live_chain_from(
            &entries,
            start,
            |e| e.get("id").and_then(Value::as_str),
            |e| e.get("parentId").and_then(Value::as_str),
            |_| false,
            is_reset,
        )
    };

    let mut turns: Vec<Turn> = Vec::new();
    // The last turn is a user turn of folded tool results.
    let mut open_results = false;
    let mut entry_turn: HashMap<&str, usize> = HashMap::new();
    let mut blobs = 0usize;
    let mut compaction: Option<&Value> = None;

    for &i in &chain {
        let e = &entries[i];
        let touched: Option<usize> = match str_of(e, "type") {
            Some("message") => {
                let Some(msg) = e.get("message") else {
                    continue;
                };
                let ts = entry_timestamp(e, Some(msg));
                match str_of(msg, "role") {
                    Some("user") => {
                        let blocks = user_blocks(msg.get("content"), &mut blobs);
                        if blocks.is_empty() {
                            None
                        } else {
                            open_results = false;
                            turns.push(Turn {
                                role: Role::User,
                                timestamp: ts,
                                model: None,
                                blocks,
                            });
                            Some(turns.len() - 1)
                        }
                    }
                    Some("assistant") => {
                        let blocks = assistant_blocks(msg.get("content"));
                        if blocks.is_empty() {
                            None
                        } else {
                            open_results = false;
                            turns.push(Turn {
                                role: Role::Assistant,
                                timestamp: ts,
                                model: assistant_model(msg),
                                blocks,
                            });
                            Some(turns.len() - 1)
                        }
                    }
                    Some("toolResult") => {
                        let block = Block::ToolResult {
                            call_id: str_of(msg, "toolCallId").unwrap_or_default().to_string(),
                            name: str_of(msg, "toolName")
                                .filter(|n| !n.is_empty())
                                .map(str::to_string),
                            text: tool_result_text(msg.get("content")),
                            is_error: msg.get("isError").and_then(Value::as_bool).unwrap_or(false),
                        };
                        if open_results {
                            if let Some(last) = turns.last_mut() {
                                last.blocks.push(block);
                            }
                        } else {
                            open_results = true;
                            turns.push(Turn {
                                role: Role::User,
                                timestamp: ts,
                                model: None,
                                blocks: vec![block],
                            });
                        }
                        Some(turns.len() - 1)
                    }
                    // developer, bashExecution, pythonExecution, fileMention,
                    // hookMessage: harness-internal, not conversation.
                    _ => None,
                }
            }
            Some("compaction") => {
                compaction = Some(e);
                None
            }
            // custom, custom_message, model_change, thinking_level_change,
            // title_change, model_usage, credential_pin, label, ttsr_injection,
            // mode_change, service_tier_change, session_init, branch_summary.
            _ => None,
        };
        if let Some(id) = str_of(e, "id") {
            entry_turn.insert(id, touched.unwrap_or(turns.len()));
        }
    }

    let summary = compaction.and_then(|c| {
        let text = str_of(c, "summary").filter(|s| !s.trim().is_empty())?;
        let covers = str_of(c, "firstKeptEntryId")
            .and_then(|id| entry_turn.get(id).copied())
            .unwrap_or(0);
        Some(Summary {
            origin: if str_of(c, "method") == Some("handoff") {
                SummaryOrigin::OmpHandoff
            } else {
                SummaryOrigin::OmpCompaction
            },
            text: text.to_string(),
            at: str_of(c, "timestamp").map(str::to_string),
            covers,
        })
    });

    if turns.is_empty() {
        return Err(HandoffError::Empty { session_id });
    }

    let mut notes = Vec::new();
    if malformed > 0 {
        notes.push(format!("skipped {malformed} malformed line(s)"));
    }
    if blobs > 0 {
        notes.push(format!(
            "{blobs} image(s) stored as blob:sha256 references were not resolved and are omitted"
        ));
    }

    Ok(Conversation {
        harness: Harness::Omp,
        session_id,
        source_kind: SourceKind::Raw,
        source_path: source_path.map(str::to_string),
        cwd: str_of(&header, "cwd").map(str::to_string),
        title: slot_title.or_else(|| {
            str_of(&header, "title")
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        }),
        git_branch: None,
        model: turns
            .iter()
            .rev()
            .find(|t| t.role == Role::Assistant)
            .and_then(|t| t.model.clone()),
        started_at: turns.first().and_then(|t| t.timestamp.clone()),
        ended_at: turns.last().and_then(|t| t.timestamp.clone()),
        summary,
        turns,
        notes,
    })
}

// ---------------------------------------------------------------------------
// Writing

/// A session file ready to place: `<sessions root>/<bucket>/<file_name>`.
#[derive(Debug, Clone, PartialEq)]
pub struct OmpSessionFile {
    /// Bucket directory name ([`omp_bucket_name`] of the plan's cwd).
    pub bucket: String,
    /// `<fileSafeTimestamp>_<session id>.jsonl`.
    pub file_name: String,
    pub contents: String,
}

enum UserPart {
    Text(String),
    Image { media_type: String, data: String },
}

enum AsstPart {
    Text(String),
    Call {
        id: String,
        name: String,
        args: Value,
    },
}

enum Item {
    User {
        parts: Vec<UserPart>,
        ts: String,
    },
    Assistant {
        parts: Vec<AsstPart>,
        ts: String,
    },
    Result {
        call_id: String,
        name: String,
        text: String,
        is_error: bool,
        ts: String,
    },
}

struct ResultInfo {
    text: String,
    is_error: bool,
    ts: String,
}

fn push_user(items: &mut Vec<Item>, mut parts: Vec<UserPart>, ts: &str) {
    parts.retain(|p| !matches!(p, UserPart::Text(t) if t.trim().is_empty()));
    if parts.is_empty() {
        return;
    }
    if let Some(Item::User { parts: prev, .. }) = items.last_mut() {
        prev.extend(parts);
    } else {
        items.push(Item::User {
            parts,
            ts: ts.to_string(),
        });
    }
}

fn push_assistant(items: &mut Vec<Item>, parts: Vec<AsstPart>, ts: &str) {
    if let Some(Item::Assistant { parts: prev, .. }) = items.last_mut() {
        prev.extend(parts);
    } else {
        items.push(Item::Assistant {
            parts,
            ts: ts.to_string(),
        });
    }
}

/// The plan as an alternating message list. A call becomes a real tool call
/// only when [`plan_tool_call`] maps it natively AND its result is present in
/// the plan (a dangling call would be an API error on resume); its result is
/// emitted right behind the assistant message. Every other call and result
/// becomes text. Adjacent same-role turns merge; empty parts and turns drop;
/// thinking blocks are never written.
fn build_items(plan: &TransplantPlan) -> Vec<Item> {
    let created = plan.created_at.as_str();
    let ts_of = |t: &Turn| t.timestamp.clone().unwrap_or_else(|| created.to_string());

    let mut results: HashMap<&str, ResultInfo> = HashMap::new();
    for turn in plan.turns.iter().filter(|t| t.role == Role::User) {
        for b in &turn.blocks {
            if let Block::ToolResult {
                call_id,
                text,
                is_error,
                ..
            } = b
            {
                results
                    .entry(call_id.as_str())
                    .or_insert_with(|| ResultInfo {
                        text: text.clone(),
                        is_error: *is_error,
                        ts: ts_of(turn),
                    });
            }
        }
    }

    let mut items: Vec<Item> = Vec::new();
    push_user(
        &mut items,
        vec![UserPart::Text(plan.preamble.clone())],
        created,
    );
    // original call id -> (tool name, emitted as a real call)
    let mut calls: HashMap<&str, (&str, bool)> = HashMap::new();
    // call id -> the native omp tool name its real call was written under
    let mut native_names: HashMap<&str, String> = HashMap::new();

    for turn in &plan.turns {
        let ts = ts_of(turn);
        match turn.role {
            Role::Assistant => {
                let mut parts: Vec<AsstPart> = Vec::new();
                let mut native_ids: Vec<&str> = Vec::new();
                for b in &turn.blocks {
                    match b {
                        Block::Text { text } => parts.push(AsstPart::Text(text.clone())),
                        Block::ToolCall { id, name, input } => {
                            let plan_call = plan_tool_call(Harness::Omp, name, input);
                            match plan_call {
                                ToolPlan::Native {
                                    name: native_name,
                                    input: args,
                                } if results.contains_key(id.as_str())
                                    && !calls.contains_key(id.as_str()) =>
                                {
                                    calls.insert(id.as_str(), (name.as_str(), true));
                                    native_names.insert(id.as_str(), native_name.clone());
                                    native_ids.push(id.as_str());
                                    parts.push(AsstPart::Call {
                                        id: safe_tool_id(id),
                                        name: native_name,
                                        args,
                                    });
                                }
                                _ => {
                                    calls.entry(id.as_str()).or_insert((name.as_str(), false));
                                    parts.push(AsstPart::Text(flattened_call_text(
                                        name,
                                        input,
                                        FLATTENED_CALL_CHARS,
                                    )));
                                }
                            }
                        }
                        // Thinking (signatures are not portable), user-side blocks.
                        _ => {}
                    }
                }
                parts.retain(|p| !matches!(p, AsstPart::Text(t) if t.trim().is_empty()));
                if parts.is_empty() {
                    continue;
                }
                push_assistant(&mut items, parts, &ts);
                for id in native_ids {
                    let r = &results[id];
                    items.push(Item::Result {
                        call_id: safe_tool_id(id),
                        // The call is written under its native omp name; the
                        // result must carry that same name, not the source's.
                        name: native_names
                            .remove(id)
                            .unwrap_or_else(|| "tool".to_string()),
                        text: r.text.clone(),
                        is_error: r.is_error,
                        ts: r.ts.clone(),
                    });
                }
            }
            Role::User => {
                let mut parts: Vec<UserPart> = Vec::new();
                for b in &turn.blocks {
                    match b {
                        Block::Text { text } => parts.push(UserPart::Text(text.clone())),
                        Block::Image { media_type, data } => parts.push(UserPart::Image {
                            media_type: media_type.clone(),
                            data: data.clone(),
                        }),
                        Block::ToolResult {
                            call_id,
                            name,
                            text,
                            is_error,
                        } => {
                            if calls.get(call_id.as_str()).is_some_and(|c| c.1) {
                                // Already emitted as a real toolResult.
                                continue;
                            }
                            let name = name
                                .as_deref()
                                .or_else(|| calls.get(call_id.as_str()).map(|c| c.0));
                            parts.push(UserPart::Text(flattened_result_text(
                                name,
                                text,
                                *is_error,
                                text.chars().count(),
                            )));
                        }
                        _ => {}
                    }
                }
                push_user(&mut items, parts, &ts);
            }
        }
    }
    items
}

/// An ordered JSON object line (omp's own key order: `type` first), so the
/// title slot and entries look like omp's.
fn obj_line(pairs: &[(&str, Value)]) -> String {
    let mut s = String::from("{");
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&Value::from(*k).to_string());
        s.push(':');
        s.push_str(&v.to_string());
    }
    s.push('}');
    s
}

/// The 256-byte title slot line, newline NOT included: the title is cut by
/// characters until the unpadded line fits, then the `pad` field is filled
/// with spaces so the line plus its `\n` is exactly [`TITLE_SLOT_BYTES`].
fn title_slot(title: &str, updated_at: &str) -> String {
    let mut t: String = title.chars().take(TITLE_SLOT_BYTES).collect();
    loop {
        let line = obj_line(&[
            ("type", json!("title")),
            ("v", json!(1)),
            ("title", json!(t)),
            ("source", json!("user")),
            ("updatedAt", json!(updated_at)),
            ("pad", json!("")),
        ]);
        if line.len() < TITLE_SLOT_BYTES {
            let pad = TITLE_SLOT_BYTES - 1 - line.len();
            // The line ends `"pad":""}`; the spaces go between the quotes.
            let cut = line.len() - 2;
            return format!("{}{}{}", &line[..cut], " ".repeat(pad), &line[cut..]);
        }
        if t.pop().is_none() {
            return line;
        }
    }
}

struct Chain {
    lines: Vec<String>,
    counter: u32,
    parent: Option<String>,
}

impl Chain {
    /// Append one tree entry chained to the previous one.
    fn push(&mut self, ty: &str, ts: &str, extra: Vec<(&'static str, Value)>) {
        self.counter += 1;
        let id = format!("{:08x}", self.counter);
        let mut pairs: Vec<(&str, Value)> = vec![
            ("type", json!(ty)),
            ("id", json!(id)),
            ("parentId", json!(self.parent)),
            ("timestamp", json!(ts)),
        ];
        pairs.extend(extra);
        self.lines.push(obj_line(&pairs));
        self.parent = Some(id);
    }
}

fn ms_of(ts: &str, fallback: &str) -> i64 {
    DateTime::parse_from_rfc3339(ts)
        .or_else(|_| DateTime::parse_from_rfc3339(fallback))
        .map_or(0, |d| d.timestamp_millis())
}

fn text_part(t: &str) -> Value {
    json!({ "type": "text", "text": t })
}

fn zero_usage() -> Value {
    json!({
        "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
        "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 }
    })
}

/// Write a [`TransplantPlan`] as a NEW omp session (title slot, header,
/// `model_change`, then messages chained by `parentId`). Never writes
/// `session_exit` (omp would treat a synthetic file ending in one as an
/// interrupted run) and never `thinking` blocks. Assistant messages carry
/// `api`/`provider`/`model` only when the plan's target model is
/// `provider/model` on a provider whose api is known (anthropic); otherwise
/// omp treats them as foreign turns and normalises them at request time,
/// which is always safe for synthetic content.
pub fn write_omp_session(plan: &TransplantPlan, home: &str, tmpdir: &str) -> OmpSessionFile {
    let created = plan.created_at.as_str();
    let short: String = plan.source_session_id.chars().take(8).collect();
    let title = plan
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map_or_else(
            || format!("handoff from {} {short}", plan.source.as_str()),
            str::to_string,
        );

    let mut chain = Chain {
        lines: vec![
            title_slot(&title, created),
            obj_line(&[
                ("type", json!("session")),
                ("version", json!(3)),
                ("id", json!(plan.new_session_id)),
                ("timestamp", json!(created)),
                ("cwd", json!(plan.cwd)),
                ("title", json!(title)),
                ("titleSource", json!("user")),
            ]),
        ],
        counter: 0,
        parent: None,
    };

    let target = plan
        .target_model
        .as_deref()
        .and_then(|m| m.split_once('/'))
        .filter(|(p, m)| !p.is_empty() && !m.is_empty());
    if let Some((provider, model)) = target {
        chain.push(
            "model_change",
            created,
            vec![("model", json!(format!("{provider}/{model}")))],
        );
    }

    for item in build_items(plan) {
        match item {
            Item::User { parts, ts } => {
                let content: Vec<Value> = parts
                    .iter()
                    .map(|p| match p {
                        UserPart::Text(t) => text_part(t),
                        UserPart::Image { media_type, data } => {
                            json!({ "type": "image", "data": data, "mimeType": media_type })
                        }
                    })
                    .collect();
                let mut msg = Map::new();
                msg.insert("role".into(), json!("user"));
                msg.insert("content".into(), Value::Array(content));
                msg.insert("attribution".into(), json!("user"));
                msg.insert("timestamp".into(), json!(ms_of(&ts, created)));
                chain.push("message", &ts, vec![("message", Value::Object(msg))]);
            }
            Item::Assistant { parts, ts } => {
                let has_call = parts.iter().any(|p| matches!(p, AsstPart::Call { .. }));
                let content: Vec<Value> = parts
                    .into_iter()
                    .map(|p| match p {
                        AsstPart::Text(t) => text_part(&t),
                        AsstPart::Call { id, name, args } => json!({
                            "type": "toolCall", "id": id, "name": name, "arguments": args,
                        }),
                    })
                    .collect();
                let mut msg = Map::new();
                msg.insert("role".into(), json!("assistant"));
                msg.insert("content".into(), Value::Array(content));
                if let Some((provider, model)) = target {
                    if provider == "anthropic" {
                        msg.insert("api".into(), json!("anthropic-messages"));
                    }
                    msg.insert("provider".into(), json!(provider));
                    msg.insert("model".into(), json!(model));
                }
                msg.insert("usage".into(), zero_usage());
                msg.insert(
                    "stopReason".into(),
                    json!(if has_call { "toolUse" } else { "stop" }),
                );
                msg.insert("timestamp".into(), json!(ms_of(&ts, created)));
                chain.push("message", &ts, vec![("message", Value::Object(msg))]);
            }
            Item::Result {
                call_id,
                name,
                text,
                is_error,
                ts,
            } => {
                let body = if text.is_empty() {
                    "(empty)"
                } else {
                    text.as_str()
                };
                let mut msg = Map::new();
                msg.insert("role".into(), json!("toolResult"));
                msg.insert("toolCallId".into(), json!(call_id));
                msg.insert("toolName".into(), json!(name));
                msg.insert("content".into(), Value::Array(vec![text_part(body)]));
                msg.insert("isError".into(), json!(is_error));
                msg.insert("timestamp".into(), json!(ms_of(&ts, created)));
                chain.push("message", &ts, vec![("message", Value::Object(msg))]);
            }
        }
    }

    let mut contents = chain.lines.join("\n");
    contents.push('\n');
    let file_safe = created.replace([':', '.'], "-");
    OmpSessionFile {
        bucket: omp_bucket_name(&plan.cwd, home, tmpdir),
        file_name: format!("{file_safe}_{}.jsonl", plan.new_session_id),
        contents,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jl(lines: &[Value]) -> String {
        let mut s = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        s.push('\n');
        s
    }

    fn msg_entry(id: &str, parent: Option<&str>, ts: &str, message: Value) -> Value {
        json!({"type": "message", "id": id, "parentId": parent, "timestamp": ts, "message": message})
    }

    fn user(text: &str) -> Value {
        json!({"role": "user", "content": [{"type": "text", "text": text}], "attribution": "user", "timestamp": 1794841202000i64})
    }

    fn fixture() -> String {
        let mut s = jl(&[
            json!({"type": "title", "v": 1, "title": "Fresh title", "source": "user", "updatedAt": "2026-11-16T15:00:00.000Z", "pad": "   "}),
            json!({"type": "session", "version": 3, "id": "01a1e4a1-0d74-7548-9455-e37ffe71b0a1", "timestamp": "2026-11-16T15:00:00.000Z", "cwd": "/home/u/project/kb", "title": "Old title"}),
            json!({"type": "model_change", "id": "m1", "parentId": null, "timestamp": "2026-11-16T15:00:01.000Z", "model": "anthropic/claude-opus-5-5"}),
            msg_entry(
                "u1",
                Some("m1"),
                "2026-11-16T15:00:02.000Z",
                user("fix the bug"),
            ),
            json!({"type": "custom", "id": "c1", "parentId": "u1", "timestamp": "2026-11-16T15:00:02.500Z", "customType": "kb.recall", "data": {}}),
            json!({"type": "custom_message", "id": "cm1", "parentId": "c1", "timestamp": "2026-11-16T15:00:02.600Z", "customType": "kb-memory-context", "content": "noise", "display": false}),
            msg_entry(
                "a1",
                Some("cm1"),
                "2026-11-16T15:00:03.000Z",
                json!({
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "hmm", "thinkingSignature": "CAQS"},
                        {"type": "redactedThinking", "data": "xx"},
                        {"type": "text", "text": "looking"},
                        {"type": "toolCall", "id": "call_x|fc_y", "name": "bash", "arguments": {"command": "ls"}},
                        {"type": "toolCall", "id": "toolu_2", "name": "read", "arguments": "{\"path\":\"a.rs\"}"},
                        {"type": "toolCall", "id": "toolu_3", "name": "eval", "arguments": "not json"}
                    ],
                    "api": "openai-codex-responses", "provider": "openai-codex", "model": "gpt-5.2",
                    "stopReason": "toolUse", "timestamp": 1794841203000i64
                }),
            ),
            // Abandoned branch: a sibling of the tool results, never on the leaf path.
            msg_entry(
                "ab",
                Some("a1"),
                "2026-11-16T15:00:03.500Z",
                user("abandoned"),
            ),
            msg_entry(
                "r1",
                Some("a1"),
                "2026-11-16T15:00:04.000Z",
                json!({
                    "role": "toolResult", "toolCallId": "call_x|fc_y", "toolName": "bash",
                    "content": [{"type": "text", "text": "a.rs"}, {"type": "text", "text": "b.rs"}],
                    "isError": false, "timestamp": 1794841204000i64
                }),
            ),
            msg_entry(
                "r2",
                Some("r1"),
                "2026-11-16T15:00:04.100Z",
                json!({
                    "role": "toolResult", "toolCallId": "toolu_2", "toolName": "read",
                    "content": [{"type": "text", "text": "fn main() {}"}],
                    "isError": true, "timestamp": 1794841204100i64
                }),
            ),
            msg_entry(
                "a2",
                Some("r2"),
                "2026-11-16T15:00:05.000Z",
                json!({
                    "role": "assistant", "content": [{"type": "text", "text": "done"}],
                    "stopReason": "stop", "timestamp": 1794841205000i64
                }),
            ),
            msg_entry(
                "u2",
                Some("a2"),
                "2026-11-16T15:00:06.000Z",
                user("now compact"),
            ),
            json!({"type": "compaction", "id": "k1", "parentId": "u2", "timestamp": "2026-11-16T15:00:07.000Z",
                   "summary": "SUMMARY", "firstKeptEntryId": "u2", "tokensBefore": 9000, "method": "handoff"}),
            msg_entry(
                "a3",
                Some("k1"),
                "2026-11-16T15:00:08.000Z",
                json!({
                    "role": "assistant", "content": [{"type": "text", "text": "ok"}],
                    "provider": "anthropic", "model": "claude-opus-5-5",
                    "stopReason": "stop", "timestamp": 1794841208000i64
                }),
            ),
        ]);
        s.insert_str(0, "this is not json\n");
        s
    }

    #[test]
    fn reads_the_live_chain_with_title_summary_and_folded_results() {
        // The garbage first line is skipped; the header is still the first record.
        let conv = read_omp_session(&fixture(), Some("/x/s.jsonl")).unwrap();
        assert_eq!(conv.session_id, "01a1e4a1-0d74-7548-9455-e37ffe71b0a1");
        assert_eq!(conv.cwd.as_deref(), Some("/home/u/project/kb"));
        assert_eq!(conv.title.as_deref(), Some("Fresh title"));
        assert_eq!(conv.source_kind, SourceKind::Raw);
        assert_eq!(conv.source_path.as_deref(), Some("/x/s.jsonl"));
        assert_eq!(conv.git_branch, None);
        assert_eq!(conv.model.as_deref(), Some("anthropic/claude-opus-5-5"));
        assert_eq!(conv.started_at.as_deref(), Some("2026-11-16T15:00:02.000Z"));
        assert_eq!(conv.ended_at.as_deref(), Some("2026-11-16T15:00:08.000Z"));
        assert_eq!(conv.notes, vec!["skipped 1 malformed line(s)".to_string()]);

        let roles: Vec<Role> = conv.turns.iter().map(|t| t.role).collect();
        assert_eq!(
            roles,
            [
                Role::User,
                Role::Assistant,
                Role::User,
                Role::Assistant,
                Role::User,
                Role::Assistant
            ]
        );
        assert_eq!(conv.turns[0].text(), "fix the bug");
        assert!(
            !conv.turns.iter().any(|t| t.text().contains("abandoned")),
            "the abandoned branch is not on the live chain"
        );

        let a1 = &conv.turns[1];
        assert_eq!(a1.model.as_deref(), Some("openai-codex/gpt-5.2"));
        assert_eq!(
            a1.blocks,
            vec![
                Block::Thinking { text: "hmm".into() },
                Block::Text {
                    text: "looking".into()
                },
                Block::ToolCall {
                    id: "call_x|fc_y".into(),
                    name: "bash".into(),
                    input: json!({"command": "ls"})
                },
                Block::ToolCall {
                    id: "toolu_2".into(),
                    name: "read".into(),
                    input: json!({"path": "a.rs"})
                },
                Block::ToolCall {
                    id: "toolu_3".into(),
                    name: "eval".into(),
                    input: json!({"_raw": "not json"})
                },
            ]
        );
        assert_eq!(
            conv.turns[2].blocks,
            vec![
                Block::ToolResult {
                    call_id: "call_x|fc_y".into(),
                    name: Some("bash".into()),
                    text: "a.rs\nb.rs".into(),
                    is_error: false
                },
                Block::ToolResult {
                    call_id: "toolu_2".into(),
                    name: Some("read".into()),
                    text: "fn main() {}".into(),
                    is_error: true
                },
            ],
            "consecutive tool results fold into one user turn"
        );
        assert_eq!(
            conv.turns[1].timestamp.as_deref(),
            Some("2026-11-16T15:00:03.000Z")
        );

        // The compaction keeps `u2` and everything after: 4 turns precede it.
        let s = conv.summary.expect("summary");
        assert_eq!(s.origin, SummaryOrigin::OmpHandoff);
        assert_eq!(s.text, "SUMMARY");
        assert_eq!(s.at.as_deref(), Some("2026-11-16T15:00:07.000Z"));
        assert_eq!(s.covers, 4);
    }

    #[test]
    fn plain_compaction_without_a_resolvable_kept_entry_covers_nothing() {
        let jsonl = jl(&[
            json!({"type": "session", "version": 3, "id": "s1", "timestamp": "2026-11-16T15:00:00.000Z", "cwd": "/w"}),
            msg_entry("u1", None, "2026-11-16T15:00:01.000Z", user("hi")),
            json!({"type": "compaction", "id": "k1", "parentId": "u1", "timestamp": "2026-11-16T15:00:02.000Z",
                   "summary": "S", "firstKeptEntryId": "gone", "tokensBefore": 1}),
            msg_entry(
                "a1",
                Some("k1"),
                "2026-11-16T15:00:03.000Z",
                json!({"role": "assistant", "content": [{"type": "text", "text": "yo"}], "timestamp": 1794841203000i64}),
            ),
        ]);
        let conv = read_omp_session(&jsonl, None).unwrap();
        let s = conv.summary.unwrap();
        assert_eq!(s.origin, SummaryOrigin::OmpCompaction);
        assert_eq!(s.covers, 0);
        assert_eq!(conv.title, None);
    }

    #[test]
    fn reset_boundary_cuts_everything_before_it() {
        let jsonl = jl(&[
            json!({"type": "session", "version": 3, "id": "s1", "timestamp": "2026-11-16T15:00:00.000Z", "cwd": "/w"}),
            msg_entry("u1", None, "2026-11-16T15:00:01.000Z", user("before clear")),
            msg_entry(
                "a1",
                Some("u1"),
                "2026-11-16T15:00:02.000Z",
                json!({"role": "assistant", "content": [{"type": "text", "text": "old"}], "timestamp": 1}),
            ),
            json!({"type": "reset_boundary", "id": "rb", "parentId": "a1", "timestamp": "2026-11-16T15:00:03.000Z"}),
            msg_entry(
                "u2",
                Some("rb"),
                "2026-11-16T15:00:04.000Z",
                user("after clear"),
            ),
            msg_entry(
                "a2",
                Some("u2"),
                "2026-11-16T15:00:05.000Z",
                json!({"role": "assistant", "content": [{"type": "text", "text": "new"}], "timestamp": 2}),
            ),
        ]);
        let conv = read_omp_session(&jsonl, None).unwrap();
        let texts: Vec<String> = conv.turns.iter().map(Turn::text).collect();
        assert_eq!(texts, ["after clear", "new"]);
    }

    #[test]
    fn v1_file_without_ids_reads_in_file_order() {
        let jsonl = jl(&[
            json!({"type": "session", "id": "old1", "timestamp": "2026-01-01T00:00:00.000Z", "cwd": "/w"}),
            json!({"type": "message", "timestamp": "2026-01-01T00:00:01.000Z", "message": user("first")}),
            json!({"type": "message", "timestamp": "2026-01-01T00:00:02.000Z",
                   "message": {"role": "assistant", "content": [{"type": "text", "text": "second"}], "model": "m"}}),
            // No entry timestamp: falls back to the message's ms epoch.
            json!({"type": "message", "message": user("third")}),
        ]);
        let conv = read_omp_session(&jsonl, None).unwrap();
        let texts: Vec<String> = conv.turns.iter().map(Turn::text).collect();
        assert_eq!(texts, ["first", "second", "third"]);
        assert_eq!(conv.model.as_deref(), Some("m"));
        assert_eq!(conv.ended_at.as_deref(), Some("2026-11-16T15:00:02.000Z"));
    }

    #[test]
    fn rejects_non_sessions_and_empty_sessions_and_notes_blobs() {
        assert!(matches!(
            read_omp_session("{\"type\":\"message\"}\n", None),
            Err(HandoffError::NotASession { .. })
        ));
        assert!(matches!(
            read_omp_session("garbage\n", None),
            Err(HandoffError::NotASession { .. })
        ));
        let header =
            json!({"type": "session", "version": 3, "id": "s1", "timestamp": "t", "cwd": "/w"});
        assert!(matches!(
            read_omp_session(&jl(std::slice::from_ref(&header)), None),
            Err(HandoffError::Empty { .. })
        ));
        let with_image = jl(&[
            header,
            msg_entry(
                "u1",
                None,
                "2026-11-16T15:00:01.000Z",
                json!({
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "look"},
                        {"type": "image", "data": "blob:sha256:abcd", "mimeType": "image/png"},
                        {"type": "image", "data": "QUJD", "mimeType": "image/jpeg"}
                    ],
                    "timestamp": 1
                }),
            ),
        ]);
        let conv = read_omp_session(&with_image, None).unwrap();
        assert_eq!(
            conv.turns[0].blocks,
            vec![
                Block::Text {
                    text: "look".into()
                },
                Block::Image {
                    media_type: "image/jpeg".into(),
                    data: "QUJD".into()
                }
            ]
        );
        assert_eq!(conv.notes.len(), 1);
        assert!(conv.notes[0].contains("blob"));
    }

    fn plan() -> TransplantPlan {
        let id = "call_x|fc_y";
        TransplantPlan {
            source: Harness::Claude,
            source_session_id: "abcdef12-0000-4000-8000-000000000000".into(),
            target: Harness::Omp,
            new_session_id: "01a1e4a1-0d74-7548-9455-e37ffe71b0a1".into(),
            cwd: "/home/u/project/kb".into(),
            title: Some("My title".into()),
            created_at: "2026-11-16T13:52:02.420Z".into(),
            preamble:
                "[kb-handoff from claude session abcdef12-0000-4000-8000-000000000000]\nGoal: ship"
                    .into(),
            turns: vec![
                Turn {
                    role: Role::User,
                    timestamp: Some("2026-11-16T13:50:00.000Z".into()),
                    model: None,
                    blocks: vec![Block::Text {
                        text: "do it".into(),
                    }],
                },
                Turn {
                    role: Role::Assistant,
                    timestamp: Some("2026-11-16T13:50:01.000Z".into()),
                    model: Some("claude-opus-5-5".into()),
                    blocks: vec![
                        Block::Thinking {
                            text: "never written".into(),
                        },
                        Block::Text { text: "ok".into() },
                        Block::ToolCall {
                            id: id.into(),
                            name: "bash".into(),
                            input: json!({"command": "ls"}),
                        },
                        Block::ToolCall {
                            id: "toolu_edit".into(),
                            name: "edit".into(),
                            input: json!({"input": "[a.rs#1A2B]\nPUT 1.=1:\n+x"}),
                        },
                    ],
                },
                Turn {
                    role: Role::User,
                    timestamp: Some("2026-11-16T13:50:02.000Z".into()),
                    model: None,
                    blocks: vec![
                        Block::ToolResult {
                            call_id: id.into(),
                            name: Some("bash".into()),
                            text: "a.rs".into(),
                            is_error: false,
                        },
                        Block::ToolResult {
                            call_id: "toolu_edit".into(),
                            name: Some("edit".into()),
                            text: "applied".into(),
                            is_error: false,
                        },
                    ],
                },
                Turn {
                    role: Role::Assistant,
                    timestamp: Some("2026-11-16T13:50:03.000Z".into()),
                    model: None,
                    blocks: vec![Block::Text {
                        text: "done".into(),
                    }],
                },
                // An empty assistant turn between two users must not break alternation.
                Turn {
                    role: Role::Assistant,
                    timestamp: None,
                    model: None,
                    blocks: vec![Block::Text { text: "  ".into() }],
                },
                Turn {
                    role: Role::User,
                    timestamp: None,
                    model: None,
                    blocks: vec![Block::Text {
                        text: "thanks".into(),
                    }],
                },
                Turn {
                    role: Role::Assistant,
                    timestamp: None,
                    model: None,
                    blocks: vec![Block::Text {
                        text: "welcome".into(),
                    }],
                },
            ],
            omitted_turns: 3,
            target_model: Some("anthropic/claude-opus-5-5".into()),
        }
    }

    fn parsed_lines(f: &OmpSessionFile) -> Vec<Value> {
        f.contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn writes_a_resumable_session_that_reads_back() {
        let f = write_omp_session(&plan(), "/home/u", "/tmp");
        assert_eq!(f.bucket, "-project-kb");
        assert_eq!(
            f.file_name,
            "2026-11-16T13-52-02-420Z_01a1e4a1-0d74-7548-9455-e37ffe71b0a1.jsonl"
        );

        let first = f.contents.split_inclusive('\n').next().unwrap();
        assert_eq!(
            first.len(),
            256,
            "title slot is exactly 256 bytes incl. newline"
        );
        let lines = parsed_lines(&f);
        assert_eq!(lines[0]["type"], "title");
        assert_eq!(lines[0]["title"], "My title");
        assert_eq!(lines[1]["type"], "session");
        assert_eq!(lines[1]["version"], 3);
        assert_eq!(lines[1]["id"], "01a1e4a1-0d74-7548-9455-e37ffe71b0a1");
        assert_eq!(lines[1]["cwd"], "/home/u/project/kb");
        assert_eq!(lines[2]["type"], "model_change");
        assert_eq!(lines[2]["model"], "anthropic/claude-opus-5-5");
        assert_eq!(lines[2]["parentId"], Value::Null);
        assert!(!f.contents.contains("session_exit"));
        assert!(!f.contents.contains("never written"));

        // parentId chain: every entry points at the one before it.
        for w in lines[2..].windows(2) {
            assert_eq!(w[1]["parentId"], w[0]["id"]);
        }

        let msgs: Vec<&Value> = lines
            .iter()
            .filter(|l| l["type"] == "message")
            .map(|l| &l["message"])
            .collect();
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(
            roles,
            [
                "user",
                "assistant",
                "toolResult",
                "user",
                "assistant",
                "user",
                "assistant"
            ]
        );
        // Alternation: no two assistants and no two real users in a row.
        for w in roles.windows(2) {
            assert_ne!(w[0], w[1], "{roles:?}");
        }
        // The preamble opens the first user message, merged with the first turn.
        assert_eq!(msgs[0]["attribution"], "user");
        assert!(msgs[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("[kb-handoff from claude session"));
        assert_eq!(msgs[0]["content"][1]["text"], "do it");

        // Composite id rewritten identically on the call and on its result.
        let safe = safe_tool_id("call_x|fc_y");
        assert_ne!(safe, "call_x|fc_y");
        let content = msgs[1]["content"].as_array().unwrap();
        let calls: Vec<&Value> = content.iter().filter(|c| c["type"] == "toolCall").collect();
        assert_eq!(calls.len(), 1, "only the bash call stays a real call");
        assert_eq!(calls[0]["id"], safe.as_str());
        assert_eq!(calls[0]["name"], "bash");
        assert_eq!(calls[0]["arguments"], json!({"command": "ls"}));
        assert_eq!(msgs[1]["stopReason"], "toolUse");
        assert_eq!(msgs[1]["api"], "anthropic-messages");
        assert_eq!(msgs[1]["provider"], "anthropic");
        assert_eq!(msgs[1]["model"], "claude-opus-5-5");
        assert_eq!(msgs[2]["toolCallId"], safe.as_str());
        assert_eq!(msgs[2]["toolName"], "bash");
        assert_eq!(msgs[2]["isError"], false);
        assert_eq!(msgs[4]["stopReason"], "stop");

        // The edit is flattened: call text in the assistant turn, result text in the user turn.
        assert!(content
            .iter()
            .any(|c| c["type"] == "text"
                && c["text"].as_str().unwrap().starts_with("[tool call · edit")));
        assert!(msgs[3]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("[tool result · edit]\napplied"));
        assert!(msgs.iter().all(|m| m["toolName"] != "edit"));

        // Round trip through the reader.
        let conv = read_omp_session(&f.contents, None).unwrap();
        assert_eq!(conv.session_id, "01a1e4a1-0d74-7548-9455-e37ffe71b0a1");
        assert_eq!(conv.title.as_deref(), Some("My title"));
        assert_eq!(conv.model.as_deref(), Some("anthropic/claude-opus-5-5"));
        assert_eq!(conv.turns.len(), 7);
        assert!(conv.summary.is_none());
        assert!(conv.notes.is_empty(), "{:?}", conv.notes);
        assert!(matches!(
            &conv.turns[2].blocks[0],
            Block::ToolResult { call_id, name: Some(n), .. } if *call_id == safe && n == "bash"
        ));
    }

    #[test]
    fn dangling_native_calls_flatten_and_long_titles_still_fit_the_slot() {
        let mut p = plan();
        // Drop the results turn: the bash call has no result, so it must not be written natively.
        p.turns.remove(2);
        p.title = Some("é".repeat(600));
        p.target_model = None;
        let f = write_omp_session(&p, "/home/u", "/tmp");
        assert_eq!(f.contents.split_inclusive('\n').next().unwrap().len(), 256);
        let lines = parsed_lines(&f);
        assert_ne!(
            lines[2]["type"], "model_change",
            "no provider/model, no model_change"
        );
        assert!(!f.contents.contains("\"toolCall\""));
        assert!(!f.contents.contains("\"api\""));
        assert!(f.contents.contains("[tool call · bash: ls]"));

        p.title = None;
        let f = write_omp_session(&p, "/home/u", "/tmp");
        let lines = parsed_lines(&f);
        assert_eq!(lines[0]["title"], "handoff from claude abcdef12");
    }

    #[test]
    fn bucket_names_follow_omps_rule() {
        let b = |cwd| omp_bucket_name(cwd, "/home/u", "/tmp");
        assert_eq!(b("/home/u/project/kb"), "-project-kb");
        assert_eq!(b("/home/u"), "-");
        assert_eq!(b("/tmp/x"), "-tmp-x");
        assert_eq!(b("/tmp"), "-tmp");
        assert_eq!(b("/opt/a:b"), "--opt-a-b--");
        assert_eq!(
            b("/home/user2/x"),
            "--home-user2-x--",
            "prefix match must respect path segments"
        );
    }

    #[test]
    fn locates_sessions_by_id_prefix_across_buckets_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = omp_sessions_root(dir.path());
        assert_eq!(
            locate_omp_session(&root, "01a1").unwrap(),
            Vec::<PathBuf>::new()
        );

        let a = root.join("-project-kb");
        let b = root.join("-tmp-x");
        std::fs::create_dir_all(a.join("2026-11-16T10-00-00-000Z_01a1e4a1-aaaa")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let older = a.join("2026-11-16T10-00-00-000Z_01a1e4a1-aaaa.jsonl");
        let newer = b.join("2026-11-16T11-00-00-000Z_01A1e4a1-bbbb.jsonl");
        let other = b.join("2026-11-16T12-00-00-000Z_01ffffff-cccc.jsonl");
        std::fs::write(&older, "{}\n").unwrap();
        std::fs::write(&other, "{}\n").unwrap();
        // A subagent transcript inside the session dir is not a top-level session.
        std::fs::write(
            a.join("2026-11-16T10-00-00-000Z_01a1e4a1-aaaa")
                .join("Sub.jsonl"),
            "{}\n",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&newer, "{}\n").unwrap();

        let hits = locate_omp_session(&root, "01A1E4A1").unwrap();
        assert_eq!(hits, vec![newer, older]);
        assert!(locate_omp_session(&root, "").unwrap().is_empty());
    }

    fn header(id: &str) -> Value {
        json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-11-16T15:00:00.000Z", "cwd": "/w"})
    }

    fn umsg(id: &str, parent: Option<&str>, text: &str) -> Value {
        json!({"type": "message", "id": id, "parentId": parent, "timestamp": "2026-11-16T15:00:01.000Z",
               "message": {"role": "user", "content": [{"type": "text", "text": text}], "timestamp": 1}})
    }

    fn amsg(id: &str, parent: Option<&str>, text: &str) -> Value {
        json!({"type": "message", "id": id, "parentId": parent, "timestamp": "2026-11-16T15:00:02.000Z",
               "message": {"role": "assistant", "content": [{"type": "text", "text": text}], "timestamp": 2}})
    }

    fn texts(jsonl: &str) -> Vec<String> {
        read_omp_session(jsonl, None)
            .unwrap()
            .turns
            .iter()
            .map(Turn::text)
            .collect()
    }

    fn tail_usage(active_leaf: Option<&str>) -> Value {
        let mut v = json!({"type": "model_usage", "id": "mu", "parentId": "u1", "timestamp": "2026-11-16T15:00:09.000Z"});
        if let Some(l) = active_leaf {
            v["activeLeafId"] = json!(l);
        }
        v
    }

    #[test]
    fn trailing_off_branch_entry_with_active_leaf_keeps_the_full_conversation() {
        let base = |tail: Value| {
            jl(&[
                header("s1"),
                umsg("u1", None, "first"),
                amsg("a1", Some("u1"), "second"),
                umsg("u2", Some("a1"), "third"),
                tail,
            ])
        };
        // `activeLeafId` names the real leaf: the whole chain is read.
        assert_eq!(
            texts(&base(tail_usage(Some("u2")))),
            ["first", "second", "third"]
        );
        // Without it the old behaviour holds: the walk starts at the last
        // physical entry and is cut at its early parent.
        assert_eq!(texts(&base(tail_usage(None))), ["first"]);
        // An `activeLeafId` naming no entry falls back to the last entry.
        assert_eq!(texts(&base(tail_usage(Some("nope")))), ["first"]);
    }

    #[test]
    fn live_chain_by_still_starts_at_the_last_entry_and_from_takes_a_start() {
        // (id, parent, title, reset)
        type E = (&'static str, Option<&'static str>, bool, bool);
        let es: Vec<E> = vec![
            ("t", None, true, false),
            ("a", None, false, false),
            ("b", Some("a"), false, false),
            ("x", Some("a"), false, false),
            ("c", Some("b"), false, false),
            ("late", Some("a"), false, false),
        ];
        fn id(e: &E) -> Option<&str> {
            Some(e.0)
        }
        fn parent(e: &E) -> Option<&str> {
            e.1
        }
        fn title(e: &E) -> bool {
            e.2
        }
        fn reset(e: &E) -> bool {
            e.3
        }
        assert_eq!(live_chain_by(&es, id, parent, title, reset), vec![1, 5]);
        assert_eq!(
            live_chain_from(&es, 4, id, parent, title, reset),
            vec![1, 2, 4]
        );
        assert!(live_chain_by::<E>(&[], id, parent, title, reset).is_empty());
    }

    #[test]
    fn written_tool_result_carries_the_native_tool_name() {
        let id = "toolu_bash1";
        let plan = TransplantPlan {
            source: Harness::Claude,
            source_session_id: "abcdef12-0000-4000-8000-000000000000".into(),
            target: Harness::Omp,
            new_session_id: "01a1e4a1-0d74-7548-9455-e37ffe71b0a1".into(),
            cwd: "/home/u/project/kb".into(),
            title: None,
            created_at: "2026-11-16T13:52:02.420Z".into(),
            preamble: "[kb-handoff]".into(),
            turns: vec![
                Turn {
                    role: Role::Assistant,
                    timestamp: None,
                    model: None,
                    blocks: vec![Block::ToolCall {
                        id: id.into(),
                        name: "Bash".into(),
                        input: json!({"command": "ls"}),
                    }],
                },
                Turn {
                    role: Role::User,
                    timestamp: None,
                    model: None,
                    blocks: vec![Block::ToolResult {
                        call_id: id.into(),
                        name: Some("Bash".into()),
                        text: "a.rs".into(),
                        is_error: false,
                    }],
                },
            ],
            omitted_turns: 0,
            target_model: None,
        };
        let f = write_omp_session(&plan, "/home/u", "/tmp");
        let lines: Vec<Value> = f
            .contents
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let msgs: Vec<&Value> = lines
            .iter()
            .filter(|l| l["type"] == "message")
            .map(|l| &l["message"])
            .collect();
        let call = msgs
            .iter()
            .flat_map(|m| m["content"].as_array().into_iter().flatten())
            .find(|c| c["type"] == "toolCall")
            .expect("a native call");
        assert_eq!(call["name"], "bash");
        let result = msgs
            .iter()
            .find(|m| m["role"] == "toolResult")
            .expect("a native result");
        assert_eq!(result["toolName"], "bash");
    }

    #[test]
    fn a_header_id_that_is_not_a_plain_token_is_rejected() {
        for bad in ["s`rm -rf`", "a b", "x\ny", ""] {
            let jsonl = jl(&[header(bad), umsg("u1", None, "hi")]);
            assert!(
                matches!(
                    read_omp_session(&jsonl, None),
                    Err(HandoffError::NotASession { harness: "omp", .. })
                ),
                "{bad:?}"
            );
        }
        let ok = jl(&[header("01a1e4a1-0d74"), umsg("u1", None, "hi")]);
        assert_eq!(
            read_omp_session(&ok, None).unwrap().session_id,
            "01a1e4a1-0d74"
        );
    }
}
