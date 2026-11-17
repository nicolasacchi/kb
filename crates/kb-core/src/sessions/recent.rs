//! SG-Q1 — the recent-turns helper: the last few HUMAN/ASSISTANT prose turns
//! of a LIVE Claude Code transcript, as plain text for building a recall
//! query (SG-Q2 wires the hook; nothing calls this yet).
//!
//! Not a second interpreter: the bounded tail comes from
//! [`tail::read_bootstrap_window`] and every record is interpreted by the
//! `session-view/1` engine ([`view_append`] + [`view_finish`], so the
//! still-open turn is included). This module only SELECTS from the resulting
//! turns: `Item::Prose` of non-sidechain turns. Everything else the engine
//! types differently (`MemoryInjection`, `SystemReminder`, `ToolCall`,
//! `Thinking`, `Command`, `Image`, ...) never qualifies. Two things the engine
//! does not type are handled here by a cheap line/text guard: compact
//! summaries (`isCompactSummary` records, dropped before the engine sees them)
//! and any `<system-reminder>` block or `kb-recall/1` marker line embedded in
//! otherwise real prose.
//!
//! Total: a missing/unreadable/empty/garbage file yields an empty `Vec`,
//! never an error (a hook must not break on it).

use std::path::Path;

use super::tail::read_bootstrap_window;
use super::view::{
    view_append, view_finish, Item, Role, ViewCarry, ViewEvent, RECALL_MARKER_PREFIX,
};

/// Default number of turns returned.
pub const RECENT_TURNS_DEFAULT_N: usize = 4;
/// Default per-item character cap.
pub const RECENT_TURNS_DEFAULT_ITEM_CHARS: usize = 600;
/// Default total character cap across all items.
pub const RECENT_TURNS_DEFAULT_TOTAL_CHARS: usize = 2000;

/// One recent turn's prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentTurn {
    pub role: Role,
    pub text: String,
}

/// Bounds for [`recent_turns_with`].
#[derive(Debug, Clone, Copy)]
pub struct RecentTurnsOpts {
    /// At most this many turns (the newest ones).
    pub max_turns: usize,
    /// Per-item cap in chars: an assistant item keeps its END, a human item
    /// its START.
    pub item_chars: usize,
    /// Cap on the summed chars of all returned items; the OLDEST items are
    /// dropped (or, for the boundary one, truncated) first.
    pub total_chars: usize,
}

impl Default for RecentTurnsOpts {
    fn default() -> Self {
        Self {
            max_turns: RECENT_TURNS_DEFAULT_N,
            item_chars: RECENT_TURNS_DEFAULT_ITEM_CHARS,
            total_chars: RECENT_TURNS_DEFAULT_TOTAL_CHARS,
        }
    }
}

/// [`recent_turns_with`] under the default bounds.
pub fn recent_turns(path: &Path) -> Vec<RecentTurn> {
    recent_turns_with(path, &RecentTurnsOpts::default())
}

/// The last `opts.max_turns` prose turns of the transcript at `path`, oldest
/// first (newest last). See the module docs for what is excluded.
pub fn recent_turns_with(path: &Path, opts: &RecentTurnsOpts) -> Vec<RecentTurn> {
    let Ok((window, _)) = read_bootstrap_window(path) else {
        return Vec::new();
    };
    recent_turns_from_window(&window, opts)
}

/// Pure core over an already-read window of complete JSONL lines.
pub fn recent_turns_from_window(window: &str, opts: &RecentTurnsOpts) -> Vec<RecentTurn> {
    if opts.max_turns == 0 || opts.item_chars == 0 || opts.total_chars == 0 {
        return Vec::new();
    }
    let mut filtered = String::with_capacity(window.len());
    for line in window.lines() {
        if is_compact_summary(line) {
            continue;
        }
        filtered.push_str(line);
        filtered.push('\n');
    }

    let (mut events, mut carry) = view_append(ViewCarry::new(), &filtered);
    events.extend(view_finish(&mut carry));
    let mut turns: Vec<_> = events
        .into_iter()
        .filter_map(|e| match e {
            ViewEvent::TurnClosed(t) => Some(t),
            ViewEvent::TaskUpdated(_) => None,
        })
        .collect();
    turns.sort_by_key(|t| t.ordinal);

    let mut items: Vec<RecentTurn> = Vec::new();
    for t in turns.iter().filter(|t| !t.sidechain) {
        let mut parts: Vec<String> = Vec::new();
        for it in &t.items {
            if let Item::Prose { text } = it {
                let clean = clean_prose(text);
                if !clean.is_empty() && clean != "[image attachment]" {
                    parts.push(clean);
                }
            }
        }
        if parts.is_empty() {
            continue;
        }
        items.push(RecentTurn {
            role: t.role,
            text: parts.join("\n"),
        });
    }

    let skip = items.len().saturating_sub(opts.max_turns);
    let mut kept: Vec<RecentTurn> = items.into_iter().skip(skip).collect();
    for it in &mut kept {
        it.text = cap(&it.text, opts.item_chars, it.role == Role::Assistant);
    }

    // Total cap: fill from the newest backwards.
    let mut remaining = opts.total_chars;
    let mut out_rev: Vec<RecentTurn> = Vec::new();
    for mut it in kept.into_iter().rev() {
        if remaining == 0 {
            break;
        }
        let n = it.text.chars().count();
        if n > remaining {
            it.text = cap(&it.text, remaining, it.role == Role::Assistant);
            remaining = 0;
        } else {
            remaining -= n;
        }
        out_rev.push(it);
    }
    out_rev.reverse();
    out_rev
}

fn is_compact_summary(line: &str) -> bool {
    if !line.contains("\"isCompactSummary\"") {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(line.trim())
        .ok()
        .and_then(|v| v.get("isCompactSummary").and_then(|x| x.as_bool()))
        .unwrap_or(false)
}

/// Drop embedded `<system-reminder>…</system-reminder>` blocks (an unclosed
/// one drops to the end) and any line carrying a `kb-recall/1` marker.
fn clean_prose(text: &str) -> String {
    const OPEN: &str = "<system-reminder>";
    const CLOSE: &str = "</system-reminder>";
    let mut s = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(OPEN) {
        s.push_str(&rest[..i]);
        match rest[i..].find(CLOSE) {
            Some(j) => rest = &rest[i + j + CLOSE.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    s.push_str(rest);
    s.lines()
        .filter(|l| !l.contains(RECALL_MARKER_PREFIX))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Cap to `max` chars, keeping the END (`keep_end`) or the START, never
/// splitting a char.
fn cap(text: &str, max: usize, keep_end: bool) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    if keep_end {
        text.chars().skip(n - max).collect()
    } else {
        text.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn user(uuid: &str, text: &str) -> String {
        serde_json::json!({"type":"user","uuid":uuid,"isSidechain":false,"promptSource":"typed",
            "message":{"role":"user","content":text},"timestamp":"2026-11-17T09:00:00.000Z"})
        .to_string()
    }
    fn asst(uuid: &str, req: &str, blocks: serde_json::Value) -> String {
        serde_json::json!({"type":"assistant","uuid":uuid,"requestId":req,"isSidechain":false,
            "message":{"role":"assistant","content":blocks},"timestamp":"2026-11-17T09:00:01.000Z"})
        .to_string()
    }
    fn text(uuid: &str, req: &str, t: &str) -> String {
        asst(uuid, req, serde_json::json!([{"type":"text","text":t}]))
    }
    fn run(lines: &[String]) -> Vec<RecentTurn> {
        recent_turns_from_window(&(lines.join("\n") + "\n"), &RecentTurnsOpts::default())
    }
    fn texts(v: &[RecentTurn]) -> Vec<&str> {
        v.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn ordered_newest_last_and_open_turn_included() {
        let v = run(&[
            user("10000000-0000-4000-8000-000000000001", "first question"),
            text("10000000-0000-4000-8000-000000000002", "r1", "first answer"),
            user("10000000-0000-4000-8000-000000000003", "second question"),
            text("10000000-0000-4000-8000-000000000004", "r2", "still typing"),
        ]);
        assert_eq!(
            texts(&v),
            [
                "first question",
                "first answer",
                "second question",
                "still typing"
            ]
        );
        assert_eq!(v[3].role, Role::Assistant);
        assert_eq!(v[0].role, Role::Human);
    }

    #[test]
    fn max_turns_keeps_the_newest() {
        let opts = RecentTurnsOpts {
            max_turns: 2,
            ..Default::default()
        };
        let w = [
            user("10000000-0000-4000-8000-000000000001", "a"),
            text("10000000-0000-4000-8000-000000000002", "r1", "b"),
            user("10000000-0000-4000-8000-000000000003", "c"),
            text("10000000-0000-4000-8000-000000000004", "r2", "d"),
        ]
        .join("\n");
        assert_eq!(texts(&recent_turns_from_window(&w, &opts)), ["c", "d"]);
    }

    #[test]
    fn injected_recall_block_never_appears() {
        let inj = serde_json::json!({"parentUuid":null,"isSidechain":false,
            "attachment":{"type":"hook_additional_context","content":["Relevant memories from kb:\n- alpha fact  [notes]  (id aaaaaaaaaaaa)\n<!--kb-recall/1 kb=notes id=aaaaaaaaaaaa pos=1-->"],
            "hookName":"UserPromptSubmit","toolUseID":"x","hookEvent":"UserPromptSubmit"},
            "type":"attachment","uuid":"20000001-0000-4000-8000-000000000001",
            "timestamp":"2026-11-17T09:00:00.000Z"}).to_string();
        let reminder = user(
            "20000002-0000-4000-8000-000000000002",
            "<system-reminder>\nRelevant memories: alpha fact\n<!--kb-recall/1 kb=notes id=aaaaaaaaaaaa-->\n</system-reminder>",
        );
        let mixed = user(
            "20000003-0000-4000-8000-000000000003",
            "real words <system-reminder>secret alpha</system-reminder>\n<!--kb-recall/1 kb=notes id=bbbbbbbbbbbb-->",
        );
        let v = run(&[
            inj,
            reminder,
            mixed,
            text("20000004-0000-4000-8000-000000000004", "r", "ok"),
        ]);
        let all = texts(&v).join("|");
        assert!(
            !all.contains("alpha") && !all.contains("kb-recall") && !all.contains("secret"),
            "{all}"
        );
        assert_eq!(texts(&v), ["real words", "ok"]);
    }

    #[test]
    fn tools_thinking_sidechain_compact_and_commands_are_excluded() {
        let tool_use = asst(
            "30000002-0000-4000-8000-000000000002",
            "r1",
            serde_json::json!([
                {"type":"thinking","thinking":"private musings"},
                {"type":"text","text":"let me look"},
                {"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"echo SECRETCMD"}}
            ]),
        );
        let result =
            serde_json::json!({"type":"user","uuid":"30000003-0000-4000-8000-000000000003",
            "isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result",
            "tool_use_id":"tu1","content":"TOOLOUTPUT"}]}})
            .to_string();
        let side =
            serde_json::json!({"type":"assistant","uuid":"30000004-0000-4000-8000-000000000004",
            "requestId":"rs","isSidechain":true,"message":{"role":"assistant",
            "content":[{"type":"text","text":"SUBAGENT TEXT"}]}})
            .to_string();
        let compact =
            serde_json::json!({"type":"user","uuid":"30000005-0000-4000-8000-000000000005",
            "isSidechain":false,"isCompactSummary":true,"message":{"role":"user",
            "content":"This session is being continued from a previous conversation SUMMARYTEXT"}})
            .to_string();
        // Real slash-command records carry no `promptSource: typed`.
        let cmd = serde_json::json!({"type":"user","uuid":"30000006-0000-4000-8000-000000000006",
            "isSidechain":false,"message":{"role":"user","content":
            "<command-name>/clear</command-name>\n<command-args></command-args>"}})
        .to_string();
        let v = run(&[
            user("30000001-0000-4000-8000-000000000001", "do it"),
            tool_use,
            result,
            side,
            compact,
            cmd,
            text("30000007-0000-4000-8000-000000000007", "r2", "done"),
        ]);
        let all = texts(&v).join("|");
        for bad in [
            "SECRETCMD",
            "TOOLOUTPUT",
            "private",
            "SUBAGENT",
            "SUMMARYTEXT",
            "/clear",
        ] {
            assert!(!all.contains(bad), "{bad} leaked: {all}");
        }
        assert!(all.contains("do it") && all.contains("let me look") && all.contains("done"));
    }

    #[test]
    fn caps_hold_on_multibyte_text_and_keep_the_right_end() {
        let long_user = "é".repeat(50) + &"日".repeat(50);
        let long_asst = "α".repeat(50) + &"🙂".repeat(50);
        let w = [
            user("40000001-0000-4000-8000-000000000001", &long_user),
            text("40000002-0000-4000-8000-000000000002", "r", &long_asst),
        ]
        .join("\n");
        let opts = RecentTurnsOpts {
            max_turns: 4,
            item_chars: 60,
            total_chars: 100,
        };
        let v = recent_turns_from_window(&w, &opts);
        let total: usize = v.iter().map(|t| t.text.chars().count()).sum();
        assert!(total <= 100, "{total}");
        assert_eq!(v.len(), 2);
        // newest (assistant) keeps its END, whole item fits 60; user gets the rest (40).
        assert_eq!(v[1].text, "α".repeat(10) + &"🙂".repeat(50));
        assert_eq!(v[0].text.chars().count(), 40);
        assert!(v[0].text.starts_with('é'), "user keeps its START");
    }

    #[test]
    fn malformed_lines_are_skipped_and_garbage_yields_nothing_harmful() {
        let w = format!(
            "{{not json\n{}\n\n[1,2\n{}\n",
            user("50000001-0000-4000-8000-000000000001", "hello"),
            text("50000002-0000-4000-8000-000000000002", "r", "world")
        );
        let v = recent_turns_from_window(&w, &RecentTurnsOpts::default());
        let all = texts(&v);
        assert!(all.contains(&"hello") && all.contains(&"world"), "{all:?}");
        assert!(recent_turns_from_window("", &RecentTurnsOpts::default()).is_empty());
        assert!(recent_turns_from_window("garbage\n{{{\n", &RecentTurnsOpts::default()).is_empty());
    }

    #[test]
    fn missing_or_empty_file_is_empty_not_an_error() {
        assert!(recent_turns(Path::new("/nonexistent/kb-sgq1/t.jsonl")).is_empty());
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("empty.jsonl");
        std::fs::write(&p, "").unwrap();
        assert!(recent_turns(&p).is_empty());
    }

    #[test]
    fn transcript_longer_than_the_bootstrap_window_still_yields_the_last_turns() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("big.jsonl");
        let mut f = std::fs::File::create(&p).unwrap();
        let filler = "x".repeat(900);
        let mut i = 0u32;
        while f.metadata().unwrap().len() < super::super::tail::TAIL_BOOTSTRAP_BYTES + 400_000 {
            writeln!(
                f,
                "{}",
                user(&format!("6{i:07}-0000-4000-8000-000000000001"), &filler)
            )
            .unwrap();
            i += 1;
        }
        writeln!(
            f,
            "{}",
            user("70000001-0000-4000-8000-000000000001", "the last question")
        )
        .unwrap();
        writeln!(
            f,
            "{}",
            text(
                "70000002-0000-4000-8000-000000000002",
                "rz",
                "the last answer"
            )
        )
        .unwrap();
        drop(f);
        let v = recent_turns(&p);
        assert!(v.len() <= RECENT_TURNS_DEFAULT_N);
        let t = texts(&v);
        assert_eq!(t[t.len() - 2..], ["the last question", "the last answer"]);
    }
}
