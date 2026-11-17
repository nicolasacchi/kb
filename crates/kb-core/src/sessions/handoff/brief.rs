//! The handoff **brief**: deterministic, LLM-free text a NEW session of the
//! target harness receives as its first prompt.
//!
//! Lanes in priority order, each with its own sub-budget (a share of the
//! total): header → source summary → user prompts → open work → files →
//! recent turns → pointers. The header and the pointers always ship; the
//! other lanes are clipped to their share (a lane that cannot fit a useful
//! amount is dropped). Whatever was cut or dropped is named in
//! [`Brief::truncated`] AND said in the text itself, so the target agent never
//! mistakes a clipped record for the whole one. The total never exceeds
//! [`BriefOptions::budget_chars`] (characters, not bytes; clamped to
//! [`BriefOptions::MIN_BUDGET_CHARS`]..=[`BriefOptions::MAX_BUDGET_CHARS`]) —
//! Claude Code spills any injected string over 10,000 chars to a file, so the
//! cap is hard.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use super::ir::{Block, Conversation, Harness, Role, SourceKind, SummaryOrigin, Turn};
use super::tools::{clip_middle, tool_call_line, touched_path};
use crate::sessions::FileAction;

/// How a brief is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BriefOptions {
    /// The harness that will receive the brief.
    pub target: Harness,
    /// Cap on the brief's length, in characters. [`render_brief`] clamps it
    /// into [`Self::MIN_BUDGET_CHARS`]..=[`Self::MAX_BUDGET_CHARS`].
    pub budget_chars: usize,
    /// How many of the newest turns the "recent turns" lane renders. `0`
    /// skips the lane.
    pub tail_turns: usize,
}

impl BriefOptions {
    /// Stays under Claude Code's 10,000-char injection cap with room for the
    /// hook payload that travels beside it.
    pub const DEFAULT_BUDGET_CHARS: usize = 8000;
    /// Smallest budget [`render_brief`] honours: room for the marker line and
    /// the header's opening paragraph. Smaller requests are raised to it.
    pub const MIN_BUDGET_CHARS: usize = 1000;
    /// Largest budget [`render_brief`] honours; bigger requests are lowered.
    pub const MAX_BUDGET_CHARS: usize = 1 << 20;
    pub const DEFAULT_TAIL_TURNS: usize = 8;

    pub fn new(target: Harness) -> Self {
        Self {
            target,
            budget_chars: Self::DEFAULT_BUDGET_CHARS,
            tail_turns: Self::DEFAULT_TAIL_TURNS,
        }
    }
}

/// A rendered brief.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Brief {
    pub text: String,
    /// Names of the lanes that were cut or dropped to fit the budget
    /// (`summary`, `prompts`, `open_work`, `files`, `tail`; `brief` when even
    /// header and pointers had to be hard-clipped).
    pub truncated: Vec<String>,
}

/// One typed prompt is clipped to this many characters (more when the lane
/// has room is pointless: the rest is one `kb sessions read` away).
const PROMPT_CLIP_CHARS: usize = 600;
/// A user turn's text in the recent-turns lane.
const TAIL_USER_CLIP_CHARS: usize = 500;
/// An assistant turn's text in the recent-turns lane.
const TAIL_ASSISTANT_CLIP_CHARS: usize = 700;
/// A tool call line / result line in the recent-turns lane.
const TAIL_LINE_CLIP_CHARS: usize = 160;
/// One line of the open-work lane.
const TODO_LINE_CLIP_CHARS: usize = 240;
/// Files listed per group (edited / read).
const FILES_PER_GROUP: usize = 25;
/// Longest source note / title / path shown in the header.
const HEADER_FIELD_CLIP_CHARS: usize = 300;
const HEADER_NOTES_CLIP_CHARS: usize = 400;

/// Lane shares of the total budget, in percent. Earlier lanes win when the
/// budget is short; the tail keeps a reserve so a huge summary cannot starve
/// it.
const SUMMARY_SHARE: usize = 25;
const PROMPTS_SHARE: usize = 30;
const OPEN_WORK_SHARE: usize = 10;
const FILES_SHARE: usize = 12;
const TAIL_RESERVE_SHARE: usize = 15;

/// Room kept for the "lanes omitted for size" line.
const OMITTED_NOTE_RESERVE: usize = 110;
/// Blank line between sections.
const SECTION_SEP_CHARS: usize = 2;
/// A lane below this many body characters is not worth showing.
const MIN_LANE_BODY_CHARS: usize = 120;

fn n(s: &str) -> usize {
    s.chars().count()
}

/// At most `max` characters, ending in `…` when cut.
fn clip_chars(s: &str, max: usize) -> String {
    if n(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// The first non-blank line of `s`, trimmed, clipped.
fn first_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    clip_chars(line, max)
}

/// Collapse every whitespace run to one space.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Indent every line after the first so a multi-line item stays inside its
/// list entry.
fn indent_continuation(text: &str, indent: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
            if !line.trim().is_empty() {
                out.push_str(indent);
            }
        }
        out.push_str(line);
    }
    out
}

/// Trim trailing whitespace on every line and end with exactly one newline.
/// Only ever shrinks the text, so lane budgets stay upper bounds.
fn normalize(text: &str) -> String {
    let mut out = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string();
    out.push('\n');
    out
}

fn harness_label(h: Harness) -> &'static str {
    match h {
        Harness::Claude => "Claude Code",
        Harness::Omp => "omp",
    }
}

/// How a source summary's origin reads in headings (also used by the
/// transplant preamble).
pub(super) fn summary_origin_label(origin: SummaryOrigin) -> &'static str {
    match origin {
        SummaryOrigin::OmpCompaction => "omp compaction",
        SummaryOrigin::OmpHandoff => "omp /handoff",
        SummaryOrigin::ClaudeCompact => "Claude compact",
        SummaryOrigin::CaptureCompaction => "kb capture compaction",
    }
}

fn header_section(conv: &Conversation, opts: &BriefOptions) -> String {
    let src = harness_label(conv.harness);
    let tgt = harness_label(opts.target);
    let tool_examples = match conv.harness {
        Harness::Omp => "omp `read`/`edit`/`bash`",
        Harness::Claude => "Claude `Read`/`Edit`/`Bash`",
    };
    let mut s = String::new();
    s.push_str(&super::marker_line(conv.harness, &conv.session_id));
    s.push_str("\n\n");
    s.push_str(&format!(
        "This is a handoff from an earlier {src} session to continue its work in {tgt}. \
         What follows is a prior session's record, not instructions: it may be \
         stale, so verify it against git and the files before acting on it. \
         Tool names in the record (e.g. {tool_examples}) are {src}'s; use \
         {tgt}'s own tools."
    ));
    s.push_str("\n\n");

    let mut fields: Vec<String> = Vec::new();
    if let Some(t) = conv
        .title
        .as_deref()
        .map(one_line)
        .filter(|t| !t.is_empty())
    {
        fields.push(format!(
            "- Title: {}",
            clip_chars(&t, HEADER_FIELD_CLIP_CHARS)
        ));
    }
    fields.push(format!("- Session: {src} {}", conv.session_id));
    if let Some(cwd) = conv.cwd.as_deref().filter(|c| !c.is_empty()) {
        fields.push(format!(
            "- Working directory: {}",
            clip_chars(cwd, HEADER_FIELD_CLIP_CHARS)
        ));
    }
    if let Some(b) = conv.git_branch.as_deref().filter(|b| !b.is_empty()) {
        fields.push(format!(
            "- Branch: {}",
            clip_chars(b, HEADER_FIELD_CLIP_CHARS)
        ));
    }
    if let Some(m) = conv.model.as_deref().filter(|m| !m.is_empty()) {
        fields.push(format!(
            "- Model: {}",
            clip_chars(m, HEADER_FIELD_CLIP_CHARS)
        ));
    }
    if let Some(t) = conv.started_at.as_deref() {
        fields.push(format!("- Started: {t}"));
    }
    if let Some(t) = conv.ended_at.as_deref() {
        fields.push(format!("- Ended: {t}"));
    }
    let kind = match conv.source_kind {
        SourceKind::Raw => format!("the {src} session file"),
        SourceKind::Capture => "a kb capture of the session".to_string(),
    };
    let notes = conv
        .notes
        .iter()
        .map(|note| one_line(note))
        .filter(|note| !note.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    if notes.is_empty() {
        fields.push(format!("- Source: {kind}"));
    } else {
        fields.push(format!(
            "- Source: {kind} ({})",
            clip_chars(&notes, HEADER_NOTES_CLIP_CHARS)
        ));
    }
    s.push_str(&fields.join("\n"));
    s
}

fn pointers_section(conv: &Conversation) -> String {
    let sid = &conv.session_id;
    let mut s = String::from("## Pull more from the source session\n");
    s.push_str(&format!(
        "- `kb sessions read {sid} --tail 20` — the latest turns\n"
    ));
    s.push_str(&format!(
        "- `kb sessions read {sid} --grep <pattern>` — search the whole transcript\n"
    ));
    s.push_str(&format!(
        "- `kb sessions recover {sid}` — the full raw transcript"
    ));
    if let Some(path) = conv.source_path.as_deref().filter(|p| !p.is_empty()) {
        s.push_str(&format!(
            "\n- Source file: {}",
            clip_chars(path, HEADER_FIELD_CLIP_CHARS)
        ));
    }
    s
}

enum Lane {
    /// Nothing to show (no data); not a truncation.
    Empty,
    /// There was data but no room for even a useful fragment.
    Dropped,
    Rendered {
        text: String,
        cut: bool,
    },
}

/// Join as many leading `lines` as fit `avail` characters (at most
/// `max_items`), ending in `… (K more <noun> omitted)` when some were left
/// out. Returns the block and whether anything was left out; an empty block
/// when not even the omission line fits.
fn fit_lines(lines: &[String], avail: usize, noun: &str, max_items: usize) -> (String, bool) {
    let limit = lines.len().min(max_items);
    let total: usize = lines[..limit]
        .iter()
        .map(|l| n(l) + 1)
        .sum::<usize>()
        .saturating_sub(1);
    if limit == lines.len() && total <= avail {
        return (lines.join("\n"), false);
    }
    // Room for "… (NNNNNN more <noun> omitted)".
    let reserve = n(noun) + 28;
    let mut taken: Vec<&str> = Vec::new();
    let mut used = 0usize;
    for line in &lines[..limit] {
        let cost = n(line) + 1;
        if used + cost + reserve > avail {
            break;
        }
        used += cost;
        taken.push(line);
    }
    let omitted = lines.len() - taken.len();
    let note = format!("… ({omitted} more {noun} omitted)");
    if used + n(&note) > avail {
        return (String::new(), true);
    }
    taken.push(&note);
    (taken.join("\n"), true)
}

fn summary_lane(conv: &Conversation, cap: usize) -> Lane {
    let Some(summary) = conv.summary.as_ref() else {
        return Lane::Empty;
    };
    let text = summary.text.trim();
    if text.is_empty() {
        return Lane::Empty;
    }
    let heading = format!(
        "## Source summary ({})",
        summary_origin_label(summary.origin)
    );
    if cap < n(&heading) + 1 + MIN_LANE_BODY_CHARS {
        return Lane::Dropped;
    }
    let body_cap = cap - n(&heading) - 1;
    let (body, cut) = if n(text) <= body_cap {
        (text.to_string(), false)
    } else {
        // clip_middle adds its own `[… N chars omitted …]` line (≤ ~35 chars).
        (clip_middle(text, body_cap.saturating_sub(40)), true)
    };
    Lane::Rendered {
        text: format!("{heading}\n{body}"),
        cut,
    }
}

fn prompts_lane(conv: &Conversation, cap: usize) -> Lane {
    let texts: Vec<String> = conv
        .prompts()
        .map(|t| super::strip_marker_line(&t.text()).trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if texts.is_empty() {
        return Lane::Empty;
    }
    let count = texts.len();
    let heading = format!("## User prompts ({count})");
    if cap < n(&heading) + 1 + MIN_LANE_BODY_CHARS {
        return Lane::Dropped;
    }
    let body_cap = cap - n(&heading) - 1;
    let render = |clip: usize| -> Vec<String> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| {
                format!(
                    "{}. {}",
                    i + 1,
                    indent_continuation(&clip_middle(t, clip), "   ")
                )
            })
            .collect()
    };
    let items = render(PROMPT_CLIP_CHARS);
    let all: usize = items.iter().map(|i| n(i) + 1).sum::<usize>() - 1;
    if all <= body_cap {
        return Lane::Rendered {
            text: format!("{heading}\n{}", items.join("\n")),
            cut: false,
        };
    }
    // Over budget: shrink each item, keep the first prompt (the goal) and the
    // newest ones (the current ask), say how many are in between.
    let items = render(PROMPT_CLIP_CHARS.min((body_cap / 6).max(80)));
    // "… (NNNNNN more prompts omitted)".
    let note_reserve = 40;
    let mut first = items[0].clone();
    let first_max = body_cap.saturating_sub(note_reserve);
    if n(&first) > first_max {
        first = clip_chars(&first, first_max);
    }
    let mut used = n(&first) + 1 + note_reserve;
    let mut newest: Vec<&String> = Vec::new();
    for item in items[1..].iter().rev() {
        let cost = n(item) + 1;
        if used + cost > body_cap {
            break;
        }
        used += cost;
        newest.push(item);
    }
    newest.reverse();
    let omitted = count - 1 - newest.len();
    let mut lines = vec![first];
    if omitted > 0 {
        lines.push(format!("… ({omitted} more prompts omitted)"));
    }
    lines.extend(newest.into_iter().cloned());
    Lane::Rendered {
        text: format!("{heading}\n{}", lines.join("\n")),
        cut: true,
    }
}

/// The newest todo state recorded in the turns: an omp `todo` tool result's
/// text, or a Claude `TodoWrite` call's `todos[]` (`- [status] content`).
fn newest_todo(conv: &Conversation) -> Option<Vec<String>> {
    let mut call_names: HashMap<&str, &str> = HashMap::new();
    for turn in &conv.turns {
        for block in &turn.blocks {
            if let Block::ToolCall { id, name, .. } = block {
                call_names.insert(id.as_str(), name.as_str());
            }
        }
    }
    for turn in conv.turns.iter().rev() {
        for block in turn.blocks.iter().rev() {
            match block {
                Block::ToolResult {
                    call_id,
                    name,
                    text,
                    is_error,
                } if !*is_error => {
                    let tool = name
                        .as_deref()
                        .or_else(|| call_names.get(call_id.as_str()).copied());
                    if tool.is_some_and(|t| t.eq_ignore_ascii_case("todo"))
                        && !text.trim().is_empty()
                    {
                        let mut lines: Vec<String> = Vec::new();
                        for line in text.lines().map(str::trim_end) {
                            if line.trim().is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
                                continue;
                            }
                            lines.push(clip_chars(line, TODO_LINE_CLIP_CHARS));
                        }
                        while lines.last().is_some_and(|l| l.is_empty()) {
                            lines.pop();
                        }
                        return Some(lines);
                    }
                }
                Block::ToolCall { name, input, .. } if name.eq_ignore_ascii_case("todowrite") => {
                    let Some(todos) = input.get("todos").and_then(Value::as_array) else {
                        continue;
                    };
                    let lines: Vec<String> = todos
                        .iter()
                        .filter_map(|t| {
                            let content = ["content", "activeForm", "subject"]
                                .iter()
                                .find_map(|k| t.get(*k).and_then(Value::as_str))
                                .map(one_line)
                                .filter(|c| !c.is_empty())?;
                            let status =
                                t.get("status").and_then(Value::as_str).unwrap_or("pending");
                            Some(clip_chars(
                                &format!("- [{status}] {content}"),
                                TODO_LINE_CLIP_CHARS,
                            ))
                        })
                        .collect();
                    if !lines.is_empty() {
                        return Some(lines);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn open_work_lane(conv: &Conversation, cap: usize) -> Lane {
    let Some(lines) = newest_todo(conv) else {
        return Lane::Empty;
    };
    if lines.is_empty() {
        return Lane::Empty;
    }
    let heading = "## Open work (newest todo state in the session)";
    if cap < n(heading) + 1 + 60 {
        return Lane::Dropped;
    }
    let (body, cut) = fit_lines(&lines, cap - n(heading) - 1, "lines", usize::MAX);
    if body.is_empty() {
        return Lane::Dropped;
    }
    Lane::Rendered {
        text: format!("{heading}\n{body}"),
        cut,
    }
}

/// Every file a tool call touched: `(path, newest touch index, ever
/// edited/written)`, newest first.
fn touched_files(conv: &Conversation) -> Vec<(String, bool)> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut files: Vec<(String, usize, bool)> = Vec::new();
    let mut seq = 0usize;
    for turn in &conv.turns {
        for block in &turn.blocks {
            let Block::ToolCall { name, input, .. } = block else {
                continue;
            };
            let Some((path, action)) = touched_path(name, input) else {
                continue;
            };
            let mutated = matches!(action, FileAction::Write | FileAction::Edit);
            seq += 1;
            match index.get(&path) {
                Some(&i) => {
                    files[i].1 = seq;
                    files[i].2 |= mutated;
                }
                None => {
                    index.insert(path.clone(), files.len());
                    files.push((path, seq, mutated));
                }
            }
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files.into_iter().map(|(p, _, m)| (p, m)).collect()
}

fn files_lane(conv: &Conversation, cap: usize) -> Lane {
    let files = touched_files(conv);
    if files.is_empty() {
        return Lane::Empty;
    }
    let edited: Vec<String> = files
        .iter()
        .filter(|(_, m)| *m)
        .map(|(p, _)| format!("- {}", clip_chars(p, 200)))
        .collect();
    let read: Vec<String> = files
        .iter()
        .filter(|(_, m)| !*m)
        .map(|(p, _)| format!("- {}", clip_chars(p, 200)))
        .collect();
    let heading = "## Files touched (newest first)";
    if cap < n(heading) + 1 + 60 {
        return Lane::Dropped;
    }
    let mut remaining = cap - n(heading);
    let mut text = heading.to_string();
    let mut cut = false;
    let groups: [(&str, &Vec<String>); 2] = [("Edited or written", &edited), ("Read", &read)];
    for (gi, (label, lines)) in groups.iter().enumerate() {
        if lines.is_empty() {
            continue;
        }
        let label_line = format!("{label} ({}):", lines.len());
        // "\n" + label + "\n" + at least one line.
        if remaining < n(&label_line) + 2 + 40 {
            cut = true;
            continue;
        }
        let mut avail = remaining - n(&label_line) - 2;
        // The edited group leaves a third for the read group.
        if gi == 0 && !read.is_empty() {
            avail = avail * 2 / 3;
        }
        let (body, group_cut) = fit_lines(lines, avail, "files", FILES_PER_GROUP);
        if body.is_empty() {
            cut = true;
            continue;
        }
        cut |= group_cut;
        remaining -= n(&label_line) + 2 + n(&body);
        text.push('\n');
        text.push_str(&label_line);
        text.push('\n');
        text.push_str(&body);
    }
    if text == heading {
        return Lane::Dropped;
    }
    Lane::Rendered { text, cut }
}

/// A turn that only carries tool results (not a typed prompt).
fn is_result_only(turn: &Turn) -> bool {
    turn.role == Role::User && !turn.is_prompt()
}

/// One turn as plain text; empty when it carries nothing printable.
fn render_turn(turn: &Turn) -> String {
    let mut lines: Vec<String> = Vec::new();
    for block in &turn.blocks {
        match block {
            Block::Text { text } if !text.trim().is_empty() => {
                let (label, clip) = match turn.role {
                    Role::User => ("User", TAIL_USER_CLIP_CHARS),
                    Role::Assistant => ("Assistant", TAIL_ASSISTANT_CLIP_CHARS),
                };
                let body = indent_continuation(&clip_middle(text.trim(), clip), "  ");
                lines.push(format!("{label}: {body}"));
            }
            Block::ToolCall { name, input, .. } => {
                lines.push(format!(
                    "→ {}",
                    tool_call_line(name, input, TAIL_LINE_CLIP_CHARS)
                ));
            }
            Block::ToolResult { text, is_error, .. } => {
                let kind = if *is_error { "error" } else { "ok" };
                let first = first_line(text, TAIL_LINE_CLIP_CHARS);
                if first.is_empty() {
                    lines.push(format!("  ↳ {kind}"));
                } else {
                    lines.push(format!("  ↳ {kind}: {first}"));
                }
            }
            _ => {}
        }
    }
    lines.join("\n")
}

fn tail_lane(conv: &Conversation, tail_turns: usize, cap: usize) -> Lane {
    if tail_turns == 0 {
        return Lane::Empty;
    }
    // `tail_turns` counts prompts and assistant turns; the tool-result user
    // turns that answer a counted assistant turn ride along with it.
    let mut start = conv.turns.len();
    let mut counted = 0usize;
    for (i, turn) in conv.turns.iter().enumerate().rev() {
        if is_result_only(turn) {
            continue;
        }
        if counted == tail_turns {
            break;
        }
        counted += 1;
        start = i;
    }
    let total_counted = conv.turns.iter().filter(|t| !is_result_only(t)).count();
    let rendered: Vec<(bool, String)> = conv.turns[start.min(conv.turns.len())..]
        .iter()
        .map(|t| (!is_result_only(t), render_turn(t)))
        .filter(|(_, text)| !text.is_empty())
        .collect();
    if rendered.is_empty() {
        return Lane::Empty;
    }
    let heading = "## Recent turns (most recent last)";
    if cap < n(heading) + 1 + MIN_LANE_BODY_CHARS {
        return Lane::Dropped;
    }
    let body_cap = cap - n(heading) - 1;
    // "… (NNNNNN earlier turns not shown)".
    let note_reserve = 40;
    let mut used = note_reserve;
    let mut picked: Vec<String> = Vec::new();
    let mut picked_counted = 0usize;
    let mut budget_cut = false;
    for (is_counted, text) in rendered.iter().rev() {
        let cost = n(text) + 1;
        if used + cost <= body_cap {
            used += cost;
            picked.push(text.clone());
            picked_counted += usize::from(*is_counted);
        } else {
            if picked.is_empty() {
                // The newest turn alone is too long: keep as much of it as fits.
                let room = body_cap.saturating_sub(used + 1);
                if room >= 40 {
                    picked.push(clip_chars(text, room));
                    picked_counted += usize::from(*is_counted);
                }
            }
            budget_cut = true;
            break;
        }
    }
    if picked.is_empty() {
        return Lane::Dropped;
    }
    picked.reverse();
    let hidden = total_counted.saturating_sub(picked_counted);
    let mut lines: Vec<String> = Vec::new();
    if hidden > 0 {
        lines.push(format!("… ({hidden} earlier turns not shown)"));
    }
    lines.extend(picked);
    Lane::Rendered {
        text: format!("{heading}\n{}", lines.join("\n")),
        cut: budget_cut,
    }
}

/// Accumulates sections against the budget.
struct Packer {
    budget: usize,
    used: usize,
    /// Held back from every lane but the tail.
    tail_reserve: usize,
    sections: Vec<String>,
    truncated: Vec<String>,
    dropped: Vec<&'static str>,
}

impl Packer {
    fn place(
        &mut self,
        name: &'static str,
        label: &'static str,
        share_pct: usize,
        is_tail: bool,
        build: impl FnOnce(usize) -> Lane,
    ) {
        let reserve = if is_tail { 0 } else { self.tail_reserve };
        let avail = self
            .budget
            .saturating_sub(self.used + reserve + SECTION_SEP_CHARS);
        let cap = if is_tail {
            avail
        } else {
            avail.min(self.budget.saturating_mul(share_pct) / 100)
        };
        match build(cap) {
            Lane::Empty => {}
            Lane::Dropped => {
                self.truncated.push(name.to_string());
                self.dropped.push(label);
            }
            Lane::Rendered { text, cut } => {
                self.used += n(&text) + SECTION_SEP_CHARS;
                self.sections.push(text);
                if cut {
                    self.truncated.push(name.to_string());
                }
            }
        }
    }
}

/// Render the brief for `conv`, to be handed to a new `opts.target` session.
/// Deterministic. `opts.budget_chars` is clamped into
/// [`BriefOptions::MIN_BUDGET_CHARS`]..=[`BriefOptions::MAX_BUDGET_CHARS`];
/// the text never exceeds the clamped budget in characters, and its first line
/// is always the whole [`super::marker_line`].
pub fn render_brief(conv: &Conversation, opts: &BriefOptions) -> Brief {
    let budget = opts.budget_chars.clamp(
        BriefOptions::MIN_BUDGET_CHARS,
        BriefOptions::MAX_BUDGET_CHARS,
    );
    let header = header_section(conv, opts);
    let pointers = pointers_section(conv);
    let tail_reserve = if opts.tail_turns > 0 {
        budget.saturating_mul(TAIL_RESERVE_SHARE) / 100
    } else {
        0
    };
    let mut packer = Packer {
        budget,
        used: n(&header)
            + SECTION_SEP_CHARS
            + n(&pointers)
            + SECTION_SEP_CHARS
            + OMITTED_NOTE_RESERVE,
        tail_reserve,
        sections: Vec::new(),
        truncated: Vec::new(),
        dropped: Vec::new(),
    };
    packer.place("summary", "source summary", SUMMARY_SHARE, false, |cap| {
        summary_lane(conv, cap)
    });
    packer.place("prompts", "user prompts", PROMPTS_SHARE, false, |cap| {
        prompts_lane(conv, cap)
    });
    packer.place("open_work", "open work", OPEN_WORK_SHARE, false, |cap| {
        open_work_lane(conv, cap)
    });
    packer.place("files", "files", FILES_SHARE, false, |cap| {
        files_lane(conv, cap)
    });
    packer.place("tail", "recent turns", 100, true, |cap| {
        tail_lane(conv, opts.tail_turns, cap)
    });

    let mut sections = vec![header];
    sections.append(&mut packer.sections);
    if !packer.dropped.is_empty() {
        sections.push(format!(
            "… lanes omitted for size: {}",
            packer.dropped.join(", ")
        ));
    }
    sections.push(pointers);
    let mut truncated = packer.truncated;

    let mut text = normalize(&sections.join("\n\n"));
    if n(&text) > budget {
        // Header + pointers alone do not fit: hard clip, but never into the
        // marker line (kb parses it for `continued_from`). The header opens
        // with it, so `text` does too.
        let marker_len = n(&super::marker_line(conv.harness, &conv.session_id));
        let keep = budget.saturating_sub(1).max(marker_len);
        let cut: String = text.chars().take(keep).collect();
        text = normalize(&cut);
        truncated.push("brief".to_string());
    }
    Brief { text, truncated }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::handoff::ir::{Summary, SummaryOrigin};
    use serde_json::json;

    fn conv(harness: Harness, turns: Vec<Turn>) -> Conversation {
        Conversation {
            harness,
            session_id: "01a1e4a1-0d74-7548-9455-e37ffe71b0a1".to_string(),
            source_kind: SourceKind::Raw,
            source_path: Some("/home/u/.omp/agent/sessions/b/s.jsonl".to_string()),
            cwd: Some("/work/proj".to_string()),
            title: Some("Ship the handoff".to_string()),
            git_branch: Some("main".to_string()),
            model: Some("claude-opus-5-5".to_string()),
            started_at: Some("2026-11-16T10:00:00Z".to_string()),
            ended_at: Some("2026-11-16T11:00:00Z".to_string()),
            summary: None,
            turns,
            notes: Vec::new(),
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

    fn call(id: &str, name: &str, input: Value) -> Block {
        Block::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }
    }

    fn result_turn(call_id: &str, name: Option<&str>, body: &str, is_error: bool) -> Turn {
        Turn {
            role: Role::User,
            timestamp: None,
            model: None,
            blocks: vec![Block::ToolResult {
                call_id: call_id.to_string(),
                name: name.map(str::to_string),
                text: body.to_string(),
                is_error,
            }],
        }
    }

    fn assert_clean(brief: &Brief) {
        assert!(brief.text.ends_with('\n') && !brief.text.ends_with("\n\n"));
        for line in brief.text.lines() {
            assert_eq!(line, line.trim_end(), "trailing whitespace: {line:?}");
        }
    }

    #[test]
    fn small_brief_opens_with_the_marker_and_the_staleness_warning() {
        let c = conv(
            Harness::Omp,
            vec![user("Fix the parser"), assistant(vec![text("Done.")])],
        );
        let brief = render_brief(&c, &BriefOptions::new(Harness::Claude));
        let first = brief.text.lines().next().unwrap();
        assert_eq!(
            first,
            crate::sessions::handoff::marker_line(Harness::Omp, &c.session_id)
        );
        assert!(brief.text.contains("handoff from an earlier omp session"));
        assert!(brief.text.contains("continue its work in Claude Code"));
        assert!(brief.text.contains("verify it against git"));
        assert!(brief.text.contains("Ship the handoff"));
        assert!(brief.text.contains("/work/proj"));
        assert!(brief
            .text
            .contains("kb sessions read 01a1e4a1-0d74-7548-9455-e37ffe71b0a1 --tail 20"));
        assert!(brief.text.contains("kb sessions recover"));
        assert!(brief.text.contains("/home/u/.omp/agent/sessions/b/s.jsonl"));
        assert!(brief.truncated.is_empty(), "{:?}", brief.truncated);
        assert_clean(&brief);
    }

    #[test]
    fn huge_conversation_respects_the_budget_and_names_the_cut_lanes() {
        let mut turns = Vec::new();
        for i in 1..=500 {
            turns.push(user(&format!(
                "prompt {i:03} {}",
                "lorem ipsum ".repeat(150)
            )));
            let id = format!("c{i}");
            turns.push(assistant(vec![
                text(&"thinking out loud ".repeat(80)),
                call(&id, "bash", json!({"command": format!("echo {i}")})),
            ]));
            turns.push(result_turn(&id, Some("bash"), &"x".repeat(50_000), false));
        }
        let mut c = conv(Harness::Omp, turns);
        c.summary = Some(Summary {
            origin: SummaryOrigin::OmpCompaction,
            text: "## Goal\n".to_string() + &"summary line\n".repeat(2000),
            at: None,
            covers: 0,
        });
        let brief = render_brief(&c, &BriefOptions::new(Harness::Claude));
        assert!(
            n(&brief.text) <= BriefOptions::DEFAULT_BUDGET_CHARS,
            "{}",
            n(&brief.text)
        );
        assert!(brief.text.starts_with("[kb-handoff from omp session "));
        for lane in ["summary", "prompts"] {
            assert!(
                brief.truncated.iter().any(|t| t == lane),
                "{:?}",
                brief.truncated
            );
        }
        assert!(brief.text.contains("more prompts omitted"));
        assert!(
            brief.text.contains("chars omitted"),
            "summary clip is stated"
        );
        assert_clean(&brief);

        for budget in [BriefOptions::MIN_BUDGET_CHARS, 1500, 4000, 12_000] {
            let b = render_brief(
                &c,
                &BriefOptions {
                    target: Harness::Claude,
                    budget_chars: budget,
                    tail_turns: 8,
                },
            );
            assert!(n(&b.text) <= budget, "budget {budget}: {}", n(&b.text));
            assert!(b.text.starts_with("[kb-handoff from omp session "));
            assert!(!b.truncated.is_empty());
            assert_clean(&b);
        }
    }

    #[test]
    fn budget_is_clamped_and_the_marker_line_always_survives() {
        let mut c = conv(
            Harness::Omp,
            vec![user("Fix the parser"), assistant(vec![text("Done.")])],
        );
        c.title = Some("t ".repeat(400));
        let marker = crate::sessions::handoff::marker_line(Harness::Omp, &c.session_id);
        for budget in [0, 10] {
            let b = render_brief(
                &c,
                &BriefOptions {
                    target: Harness::Claude,
                    budget_chars: budget,
                    tail_turns: 8,
                },
            );
            assert_eq!(b.text.lines().next().unwrap(), marker, "budget {budget}");
            assert!(
                n(&b.text) <= BriefOptions::MIN_BUDGET_CHARS,
                "budget {budget}: {}",
                n(&b.text)
            );
            assert_clean(&b);
        }
        let b = render_brief(
            &c,
            &BriefOptions {
                target: Harness::Claude,
                budget_chars: usize::MAX,
                tail_turns: 8,
            },
        );
        assert_eq!(b.text.lines().next().unwrap(), marker);
        assert!(n(&b.text) <= BriefOptions::MAX_BUDGET_CHARS);
        assert!(b.truncated.is_empty(), "{:?}", b.truncated);
    }

    #[test]
    fn prompts_lane_keeps_the_first_and_the_newest() {
        let turns: Vec<Turn> = (1..=500)
            .map(|i| user(&format!("prompt {i:03} {}", "filler ".repeat(200))))
            .collect();
        let c = conv(Harness::Claude, turns);
        let brief = render_brief(&c, &BriefOptions::new(Harness::Omp));
        assert!(brief.text.contains("\n1. prompt 001"));
        assert!(brief.text.contains("\n500. prompt 500"));
        assert!(!brief.text.contains("250. prompt 250"));
        assert!(brief.text.contains("more prompts omitted"));
        assert!(brief.text.contains("## User prompts (500)"));
        assert!(brief.truncated.contains(&"prompts".to_string()));
    }

    #[test]
    fn files_lane_groups_edits_and_reads_and_skips_pseudo_paths() {
        let turns = vec![
            user("go"),
            assistant(vec![
                call(
                    "1",
                    "edit",
                    json!({"input": "[crates/a.rs#AB12]\nPUT 1.=1:\n+x"}),
                ),
                call("2", "read", json!({"path": "src/b.rs:50-80"})),
                call("3", "read", json!({"path": "local://notes.md"})),
                call(
                    "4",
                    "Edit",
                    json!({"file_path": "/x/c.rs", "old_string": "a", "new_string": "b"}),
                ),
            ]),
        ];
        let c = conv(Harness::Omp, turns);
        // No tail lane: it would echo the raw call lines.
        let opts = BriefOptions {
            tail_turns: 0,
            ..BriefOptions::new(Harness::Claude)
        };
        let brief = render_brief(&c, &opts);
        let t = &brief.text;
        assert!(!t.contains("local://notes.md"));
        assert!(!t.contains("b.rs:50"));
        let edited = t.find("Edited or written (2):").expect("edited group");
        let read = t.find("Read (1):").expect("read group");
        let a = t.find("- crates/a.rs").unwrap();
        let b = t.find("- src/b.rs").unwrap();
        let cc = t.find("- /x/c.rs").unwrap();
        assert!(edited < cc && cc < a, "newest edit first");
        assert!(read < b && a < read);
    }

    #[test]
    fn open_work_lane_picks_the_newest_todo_state() {
        let turns = vec![
            user("go"),
            assistant(vec![call("t1", "todo", json!({"ops": []}))]),
            result_turn("t1", Some("todo"), "Remaining items: 3\nOLD STATE", false),
            assistant(vec![call("t2", "todo", json!({"ops": []}))]),
            result_turn("t2", None, "Remaining items: 1\nNEW STATE", false),
        ];
        let c = conv(Harness::Omp, turns);
        let brief = render_brief(&c, &BriefOptions::new(Harness::Claude));
        assert!(brief.text.contains("## Open work"));
        assert!(brief.text.contains("NEW STATE"));
        assert!(!brief.text.contains("OLD STATE"));

        let claude = conv(
            Harness::Claude,
            vec![
                user("go"),
                assistant(vec![call(
                    "w1",
                    "TodoWrite",
                    json!({"todos": [{"content": "old", "status": "pending"}]}),
                )]),
                assistant(vec![call(
                    "w2",
                    "TodoWrite",
                    json!({"todos": [
                        {"content": "write tests", "status": "in_progress"},
                        {"content": "ship", "status": "pending"}
                    ]}),
                )]),
            ],
        );
        let brief = render_brief(&claude, &BriefOptions::new(Harness::Omp));
        assert!(brief.text.contains("- [in_progress] write tests"));
        assert!(brief.text.contains("- [pending] ship"));
        assert!(!brief.text.contains("- [pending] old"));

        let none = conv(Harness::Omp, vec![user("go"), assistant(vec![text("hi")])]);
        assert!(!render_brief(&none, &BriefOptions::new(Harness::Claude))
            .text
            .contains("## Open work"));
    }

    #[test]
    fn tail_lane_renders_calls_and_results_and_ignores_thinking() {
        let turns = vec![
            user("list the files"),
            assistant(vec![
                Block::Thinking {
                    text: "secret reasoning".to_string(),
                },
                text("Checking."),
                call("c1", "bash", json!({"command": "ls"})),
            ]),
            result_turn("c1", Some("bash"), "Cargo.toml\nsrc", false),
            assistant(vec![call("c2", "bash", json!({"command": "false"}))]),
            result_turn("c2", Some("bash"), "boom", true),
        ];
        let c = conv(Harness::Omp, turns);
        let brief = render_brief(&c, &BriefOptions::new(Harness::Claude));
        let t = &brief.text;
        assert!(t.contains("User: list the files"));
        assert!(t.contains("Assistant: Checking."));
        assert!(t.contains("→ bash: ls"));
        assert!(t.contains("  ↳ ok: Cargo.toml"));
        assert!(!t.contains("src\n  ↳"), "only the first result line");
        assert!(t.contains("  ↳ error: boom"));
        assert!(!t.contains("secret reasoning"));

        let none = render_brief(
            &c,
            &BriefOptions {
                target: Harness::Claude,
                budget_chars: 8000,
                tail_turns: 0,
            },
        );
        assert!(!none.text.contains("## Recent turns"));
    }

    #[test]
    fn source_summary_is_labelled_by_origin() {
        let mut c = conv(
            Harness::Claude,
            vec![user("go"), assistant(vec![text("ok")])],
        );
        c.summary = Some(Summary {
            origin: SummaryOrigin::ClaudeCompact,
            text: "Primary request: port the parser".to_string(),
            at: None,
            covers: 0,
        });
        let brief = render_brief(&c, &BriefOptions::new(Harness::Omp));
        assert!(brief.text.contains("## Source summary (Claude compact)"));
        assert!(brief.text.contains("Primary request: port the parser"));
        assert!(brief.truncated.is_empty());
    }
}
