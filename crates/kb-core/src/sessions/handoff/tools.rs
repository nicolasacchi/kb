//! Tool-call portability policy for handoffs.
//!
//! omp and Claude Code expose different tool sets. A transplanted history
//! that shows the target model calls to tools it does not have invites
//! imitation ("No such tool" turns), so the writers apply one rule:
//!
//! * **Native** — the three tools that map 1:1 keep a real tool-call/result
//!   pair, renamed and re-shaped for the target:
//!   shell (`bash`/`Bash` `{command}`), file read (`read{path}` /
//!   `Read{file_path, offset?, limit?}`) and file write (`write{path,content}`
//!   / `Write{file_path,content}`). A call only qualifies when its arguments
//!   carry no semantics the target tool lacks (omp `bash` with `cwd`, omp
//!   `read` of a URI or with a selector the Claude tool cannot express).
//! * **Flatten** — every other call becomes plain text in the assistant turn
//!   ([`flattened_call_text`]) and its result plain text in the next user turn
//!   ([`flattened_result_text`]).
//!
//! Also here: tool-call id sanitising ([`safe_tool_id`]), the one-line call
//! summary the brief and the flattener share ([`tool_call_line`]), and the
//! file-touch extractor the brief's files lane uses ([`touched_path`]).

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use super::ir::Harness;
use crate::sessions::FileAction;

/// What a writer does with one tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolPlan {
    /// Emit a real tool call with this target-native name and input.
    Native { name: String, input: Value },
    /// Emit the call and its result as plain text.
    Flatten,
}

/// A tool-call id valid for every target: kept verbatim when it already
/// matches `^[A-Za-z0-9_-]{1,64}$` (Anthropic's rule; omp normalises the
/// same way), else `toolu_` + the first 24 hex chars of its sha256. Pure
/// and stable, so a call and its result map to the same id.
pub fn safe_tool_id(original: &str) -> String {
    let valid = !original.is_empty()
        && original.len() <= 64
        && original
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if valid {
        return original.to_string();
    }
    let digest = hex::encode(Sha256::digest(original.as_bytes()));
    format!("toolu_{}", &digest[..24])
}

/// The portable core of a 1:1 tool call.
enum Canon {
    Shell {
        command: String,
    },
    Read {
        path: String,
        /// 1-based first line, when the source asked for a window.
        offset: Option<u64>,
        limit: Option<u64>,
    },
    Write {
        path: String,
        content: String,
    },
}

fn str_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

fn u64_field(input: &Value, key: &str) -> Option<u64> {
    input.get(key).and_then(Value::as_u64)
}

fn has_scheme(path: &str) -> bool {
    path.contains("://")
}

/// Is `s` an omp read selector (`raw`, `50`, `50-80`, `-60`, `50+150`,
/// `5-16,960-973`, `50-`)?
fn is_selector(s: &str) -> bool {
    s == "raw"
        || (!s.is_empty()
            && s.bytes().any(|b| b.is_ascii_digit())
            && s.bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b',')))
}

/// Split an omp path into `(plain path, selectors)`: trailing
/// `:<selector>` segments after the last `/` are peeled off
/// (`src/a.rs:50-80:raw` → `("src/a.rs", ["50-80", "raw"])`).
fn split_selectors(path: &str) -> (&str, Vec<&str>) {
    let mut plain = path;
    let mut sels = Vec::new();
    loop {
        let slash = plain.rfind('/').map_or(0, |i| i + 1);
        let Some(colon) = plain[slash..].rfind(':').map(|i| slash + i) else {
            break;
        };
        let tail = &plain[colon + 1..];
        if colon == 0 || !is_selector(tail) {
            break;
        }
        sels.push(tail);
        plain = &plain[..colon];
    }
    sels.reverse();
    (plain, sels)
}

fn canon(name: &str, input: &Value) -> Option<Canon> {
    match name.to_ascii_lowercase().as_str() {
        "bash" => {
            let command = str_field(input, "command")?;
            if command.trim().is_empty() {
                return None;
            }
            // omp `bash` runs in `cwd` when given; Claude's Bash has no such
            // argument, so the call would mean something else.
            if str_field(input, "cwd").is_some_and(|c| !c.is_empty()) {
                return None;
            }
            Some(Canon::Shell {
                command: command.to_string(),
            })
        }
        "read" => {
            let raw = str_field(input, "file_path").or_else(|| str_field(input, "path"))?;
            if raw.is_empty() || has_scheme(raw) {
                return None;
            }
            let (plain, sels) = split_selectors(raw);
            let (offset, limit) = match sels.as_slice() {
                [] => (u64_field(input, "offset"), u64_field(input, "limit")),
                // `:50` / `:50-80` / `:50+150` / `:50-` express as a window.
                [one] => parse_window(one)?,
                _ => return None,
            };
            Some(Canon::Read {
                path: plain.to_string(),
                offset,
                limit,
            })
        }
        "write" => {
            let path = str_field(input, "file_path").or_else(|| str_field(input, "path"))?;
            let content = str_field(input, "content")?;
            if path.is_empty() || has_scheme(path) {
                return None;
            }
            Some(Canon::Write {
                path: path.to_string(),
                content: content.to_string(),
            })
        }
        _ => None,
    }
}

/// An omp single-range selector as a Claude `(offset, limit)` window.
/// `None` for shapes Claude's Read cannot express (`raw`, `-60`, lists).
#[allow(clippy::type_complexity)]
fn parse_window(sel: &str) -> Option<(Option<u64>, Option<u64>)> {
    if let Some((a, b)) = sel.split_once('+') {
        return Some((Some(a.parse().ok()?), Some(b.parse().ok()?)));
    }
    if let Some((a, b)) = sel.split_once('-') {
        let start: u64 = a.parse().ok()?;
        if b.is_empty() {
            return Some((Some(start), None));
        }
        let end: u64 = b.parse().ok()?;
        if end < start {
            return None;
        }
        return Some((Some(start), Some(end - start + 1)));
    }
    let line: u64 = sel.parse().ok()?;
    Some((Some(line), None))
}

/// Decide how a writer for `target` emits one tool call.
pub fn plan_tool_call(target: Harness, name: &str, input: &Value) -> ToolPlan {
    let Some(c) = canon(name, input) else {
        return ToolPlan::Flatten;
    };
    let (name, input) = match (target, c) {
        (Harness::Claude, Canon::Shell { command }) => ("Bash", json!({ "command": command })),
        (Harness::Omp, Canon::Shell { command }) => ("bash", json!({ "command": command })),
        (
            Harness::Claude,
            Canon::Read {
                path,
                offset,
                limit,
            },
        ) => {
            let mut m = Map::new();
            m.insert("file_path".into(), Value::String(path));
            if let Some(o) = offset {
                m.insert("offset".into(), json!(o));
            }
            if let Some(l) = limit {
                m.insert("limit".into(), json!(l));
            }
            ("Read", Value::Object(m))
        }
        (
            Harness::Omp,
            Canon::Read {
                path,
                offset,
                limit,
            },
        ) => {
            let path = match (offset, limit) {
                (None, None) => path,
                (Some(o), Some(l)) => format!("{path}:{o}+{l}"),
                (Some(o), None) => format!("{path}:{o}-"),
                (None, Some(l)) => format!("{path}:1+{l}"),
            };
            ("read", json!({ "path": path }))
        }
        (Harness::Claude, Canon::Write { path, content }) => {
            ("Write", json!({ "file_path": path, "content": content }))
        }
        (Harness::Omp, Canon::Write { path, content }) => {
            ("write", json!({ "path": path, "content": content }))
        }
    };
    ToolPlan::Native {
        name: name.to_string(),
        input,
    }
}

/// Keep at most `max_chars` characters of `text`: the head (60%) and the
/// tail (40%) around a `[… N chars omitted …]` line. Unchanged when it fits.
pub fn clip_middle(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars {
        return text.to_string();
    }
    let head_n = max_chars * 3 / 5;
    let tail_n = max_chars.saturating_sub(head_n);
    let omitted = total - head_n - tail_n;
    let head: String = text.chars().take(head_n).collect();
    let tail: String = text.chars().skip(total - tail_n).collect();
    format!("{head}\n[… {omitted} chars omitted …]\n{tail}")
}

/// Keep at most `max_chars` characters of a single line, ending in `…` when
/// cut.
fn clip_line(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut s: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// The first `[path#TAG]` header of an omp hashline edit, or the first
/// `*** Update/Add/Delete File: path` line of an apply_patch body.
fn patch_body_path(body: &str) -> Option<&str> {
    for line in body.lines() {
        let l = line.trim();
        if let Some(inner) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            if let Some((path, _tag)) = inner.rsplit_once('#') {
                if !path.is_empty() {
                    return Some(path);
                }
            }
        }
        for p in ["*** Update File: ", "*** Add File: ", "*** Delete File: "] {
            if let Some(path) = l.strip_prefix(p) {
                return Some(path.trim());
            }
        }
    }
    None
}

/// One human line summarising a tool call: `<name>: <the argument that says
/// what it did>` (command, path, pattern, query, url …), else compact JSON.
/// Newlines are cut at the first one; the whole line is at most `max_chars`.
pub fn tool_call_line(name: &str, input: &Value, max_chars: usize) -> String {
    const KEYS: [&str; 11] = [
        "command",
        "file_path",
        "notebook_path",
        "path",
        "pattern",
        "query",
        "url",
        "prompt",
        "description",
        "assignment",
        "code",
    ];
    let detail = KEYS
        .iter()
        .find_map(|k| str_field(input, k).filter(|s| !s.trim().is_empty()))
        .map(str::to_string)
        .or_else(|| {
            str_field(input, "input")
                .and_then(patch_body_path)
                .map(str::to_string)
        })
        .or_else(|| {
            input.as_object().and_then(|m| {
                m.values()
                    .find_map(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
            })
        })
        .unwrap_or_else(|| match input {
            Value::Object(m) if m.is_empty() => String::new(),
            other => other.to_string(),
        });
    let first = detail.lines().next().unwrap_or("").trim();
    let more = if detail.trim_end().contains('\n') {
        " …"
    } else {
        ""
    };
    let line = if first.is_empty() {
        name.to_string()
    } else {
        format!("{name}: {first}{more}")
    };
    clip_line(&line, max_chars)
}

/// The text a flattened tool call becomes inside an assistant turn.
pub fn flattened_call_text(name: &str, input: &Value, max_chars: usize) -> String {
    format!("[tool call · {}]", tool_call_line(name, input, max_chars))
}

/// The text a flattened tool result becomes inside the following user turn.
pub fn flattened_result_text(
    name: Option<&str>,
    text: &str,
    is_error: bool,
    max_chars: usize,
) -> String {
    let label = if is_error {
        "tool error"
    } else {
        "tool result"
    };
    let name = name.unwrap_or("tool");
    let body = clip_middle(text.trim_end(), max_chars);
    if body.is_empty() {
        format!("[{label} · {name}] (empty)")
    } else {
        format!("[{label} · {name}]\n{body}")
    }
}

/// The file a tool call read, wrote or edited, for the brief's files lane.
/// `None` for non-file tools and for pseudo-paths (`agent://`, `local://` …).
/// omp read selectors are stripped (`a.rs:50-80` → `a.rs`).
pub fn touched_path(name: &str, input: &Value) -> Option<(String, FileAction)> {
    let lower = name.to_ascii_lowercase();
    let action = match lower.as_str() {
        "read" => FileAction::Read,
        "write" => FileAction::Write,
        "edit" | "multiedit" | "notebookedit" | "ast_edit" | "apply_patch" => FileAction::Edit,
        _ => return None,
    };
    let path = str_field(input, "file_path")
        .or_else(|| str_field(input, "notebook_path"))
        .or_else(|| str_field(input, "path"))
        .or_else(|| str_field(input, "input").and_then(patch_body_path))
        .or_else(|| str_field(input, "patch").and_then(patch_body_path))?;
    if path.is_empty() || has_scheme(path) {
        return None;
    }
    let (plain, _) = split_selectors(path);
    Some((plain.to_string(), action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_tool_id_keeps_valid_ids_and_hashes_composite_ones() {
        assert_eq!(safe_tool_id("toolu_01ABC"), "toolu_01ABC");
        let composite = "call_Xy9|fc_123.abc";
        let a = safe_tool_id(composite);
        assert!(a.starts_with("toolu_") && a.len() == 30, "{a}");
        assert_eq!(a, safe_tool_id(composite), "stable: call and result agree");
        assert_ne!(a, safe_tool_id("call_Xy9|fc_124.abc"));
        assert!(safe_tool_id(&"x".repeat(65)).starts_with("toolu_"));
        assert!(safe_tool_id("").starts_with("toolu_"));
    }

    #[test]
    fn shell_read_write_map_natively_both_ways() {
        assert_eq!(
            plan_tool_call(Harness::Claude, "bash", &json!({"command": "ls", "i": "x"})),
            ToolPlan::Native {
                name: "Bash".into(),
                input: json!({"command": "ls"})
            }
        );
        assert_eq!(
            plan_tool_call(
                Harness::Omp,
                "Bash",
                &json!({"command": "ls", "timeout": 5})
            ),
            ToolPlan::Native {
                name: "bash".into(),
                input: json!({"command": "ls"})
            }
        );
        assert_eq!(
            plan_tool_call(Harness::Claude, "read", &json!({"path": "/a/b.rs:50-80"})),
            ToolPlan::Native {
                name: "Read".into(),
                input: json!({"file_path": "/a/b.rs", "offset": 50, "limit": 31})
            }
        );
        assert_eq!(
            plan_tool_call(
                Harness::Omp,
                "Read",
                &json!({"file_path": "/a/b.rs", "offset": 10, "limit": 5})
            ),
            ToolPlan::Native {
                name: "read".into(),
                input: json!({"path": "/a/b.rs:10+5"})
            }
        );
        assert_eq!(
            plan_tool_call(
                Harness::Omp,
                "Write",
                &json!({"file_path": "x", "content": "y"})
            ),
            ToolPlan::Native {
                name: "write".into(),
                input: json!({"path": "x", "content": "y"})
            }
        );
    }

    #[test]
    fn calls_with_target_less_semantics_flatten() {
        // omp bash in another cwd, a URI read, a multi-range selector, edits.
        for (name, input) in [
            ("bash", json!({"command": "ls", "cwd": "/tmp"})),
            ("read", json!({"path": "agent://Foo"})),
            ("read", json!({"path": "a.rs:5-16,960-973"})),
            ("read", json!({"path": "a.rs:raw"})),
            ("edit", json!({"input": "[a.rs#AB12]\nPUT 1.=1:\n+x"})),
            (
                "Edit",
                json!({"file_path": "a.rs", "old_string": "a", "new_string": "b"}),
            ),
            ("eval", json!({"code": "1+1"})),
            ("Bash", json!({"command": "   "})),
        ] {
            assert_eq!(
                plan_tool_call(Harness::Claude, name, &input),
                ToolPlan::Flatten,
                "{name} {input}"
            );
        }
    }

    #[test]
    fn selectors_split_only_after_the_last_slash() {
        assert_eq!(
            split_selectors("a.rs:50-80:raw"),
            ("a.rs", vec!["50-80", "raw"])
        );
        assert_eq!(split_selectors("/x:1/b.rs"), ("/x:1/b.rs", vec![]));
        assert_eq!(
            split_selectors("notes:draft.md"),
            ("notes:draft.md", vec![])
        );
    }

    #[test]
    fn touched_path_reads_every_shape_and_skips_pseudo_paths() {
        assert_eq!(
            touched_path("read", &json!({"path": "src/a.rs:10"})),
            Some(("src/a.rs".into(), FileAction::Read))
        );
        assert_eq!(
            touched_path("edit", &json!({"input": "[src/b.rs#1A2B]\nPUT 1.=2:\n+x"})),
            Some(("src/b.rs".into(), FileAction::Edit))
        );
        assert_eq!(
            touched_path("Edit", &json!({"file_path": "/c.rs"})),
            Some(("/c.rs".into(), FileAction::Edit))
        );
        assert_eq!(
            touched_path("write", &json!({"path": "local://plan.md", "content": ""})),
            None
        );
        assert_eq!(touched_path("bash", &json!({"command": "cat x"})), None);
    }

    #[test]
    fn call_lines_pick_the_telling_argument_and_stay_one_line() {
        assert_eq!(
            tool_call_line("bash", &json!({"command": "git status\ngit log"}), 80),
            "bash: git status …"
        );
        assert_eq!(
            tool_call_line("edit", &json!({"input": "[src/b.rs#1A2B]\nPUT 1.=2:"}), 80),
            "edit: src/b.rs"
        );
        assert_eq!(tool_call_line("todo", &json!({}), 80), "todo");
        let long = tool_call_line("bash", &json!({"command": "x".repeat(200)}), 40);
        assert_eq!(long.chars().count(), 40);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn clip_middle_keeps_head_and_tail_and_counts_the_gap() {
        let t: String = ('a'..='z').cycle().take(1000).collect();
        let c = clip_middle(&t, 100);
        assert!(c.starts_with(&t[..60]));
        assert!(c.ends_with(&t[t.len() - 40..]));
        assert!(c.contains("[… 900 chars omitted …]"));
        assert_eq!(clip_middle("short", 100), "short");
    }
}
