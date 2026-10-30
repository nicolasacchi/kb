//! OL IDEA3 -- the session evidence card: which verification commands a
//! session ran after its last edit, and how they ended.
//!
//! PURE and clock-free (no I/O): a CLOSED grammar of runners over the Bash
//! tool calls of a transcript's MAIN thread (no in-daemon LLM, in the style of
//! R4). The grammar never guesses: an unmatched command is simply not a check,
//! and a run whose result it cannot attribute is `unknown`, never `passed`.
//!
//! Three layers:
//!
//! 1. the command grammar ([`split_command`], [`strip_wrappers`],
//!    [`classify_command`], the output grammar),
//! 2. the transcript [`Tracker`] (three calls from the activity parse loop)
//!    and the outcome ladder [`resolve`],
//! 3. the persisted [`SessionEvidence`] ([`build_evidence`], written to
//!    `sessions.evidence_json`, V0046) and the read-time [`summarize`] verdict
//!    with its ONE copy table.
//!
//! The verdict is SURFACED, NEVER SCORED: it is not a sort key, a filter, or a
//! term in the R1 digest, the `code` lane, recall, recollect or the inbox
//! (pinned by `no_ranking_module_reads_session_evidence`).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::constants::HARNESSES;
use super::truncate_chars_ellipsis;
use crate::timeparse::parse_iso_utc;

/// Persisted-shape version of [`SessionEvidence`].
pub const EVIDENCE_VERSION: u32 = 1;
/// How many of the newest checks the persisted evidence keeps.
pub const MAX_CHECKS_KEPT: usize = 16;
/// How many unnamed-edit basenames the persisted evidence keeps.
pub const MAX_UNNAMED_EDITS: usize = 12;
/// Cap (chars) on a check's `cmd`.
pub const CMD_CAP_CHARS: usize = 200;
/// Cap (chars) on a check's `label`.
pub const LABEL_CAP_CHARS: usize = 80;

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// The read-time verdict over a capture's evidence.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "EvidenceVerdict")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// A check passed after the last edit and none failed.
    Verified,
    /// A check failed after the last edit.
    Failing,
    /// Checks ran after the last edit but none ended decisively.
    Unconfirmed,
    /// The last check ran before the last edit.
    Stale,
    /// Edits, and no check at all.
    Unchecked,
    /// No counted edit in the main transcript.
    NoEdits,
    /// The column is NULL, unparseable or a future version.
    NotExtracted,
}

/// How one check run ended.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "CheckOutcome")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    Unknown,
}

/// What a decisive outcome was read from.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "CheckBasis")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    Exit,
    Output,
    Notification,
}

/// Why an outcome is `unknown`.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "CheckWhy")
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    Piped,
    Chained,
    Json,
    Pending,
    NoExitSignal,
    NoExitCode,
    NoResult,
    Background,
    BackgroundStopped,
    Interrupted,
}

impl Why {
    /// The words the wire, the CLI and the SPA print.
    pub fn words(self) -> &'static str {
        match self {
            Why::Piped => "exit status hidden by a pipe",
            Why::Chained => "exit status mixed with other commands",
            Why::Json => "--json output carries no status",
            Why::Pending => "checks still pending",
            Why::NoExitSignal => "this harness records no exit status",
            Why::NoExitCode => "ended in an error without an exit code",
            Why::NoResult => "no result recorded",
            Why::Background => "ran in the background, no completion seen",
            Why::BackgroundStopped => "background run was stopped",
            Why::Interrupted => "interrupted",
        }
    }
}

/// A runner of the CLOSED grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runner {
    CargoTest,
    CargoNextest,
    CargoClippy,
    CargoFmtCheck,
    CargoCheck,
    CargoBuild,
    Just,
    NpmScript,
    Vitest,
    Playwright,
    Tsc,
    Pytest,
    GoTest,
    GoVet,
    GhChecks,
}

impl Runner {
    /// The stable id persisted in `runners`.
    pub fn id(self) -> &'static str {
        match self {
            Runner::CargoTest => "cargo-test",
            Runner::CargoNextest => "cargo-nextest",
            Runner::CargoClippy => "cargo-clippy",
            Runner::CargoFmtCheck => "cargo-fmt-check",
            Runner::CargoCheck => "cargo-check",
            Runner::CargoBuild => "cargo-build",
            Runner::Just => "just",
            Runner::NpmScript => "npm-script",
            Runner::Vitest => "vitest",
            Runner::Playwright => "playwright",
            Runner::Tsc => "tsc",
            Runner::Pytest => "pytest",
            Runner::GoTest => "go-test",
            Runner::GoVet => "go-vet",
            Runner::GhChecks => "gh-checks",
        }
    }
}

/// The closed runner table: `(id, what it matches)`. Documentation of the
/// grammar [`match_runner`] implements; `closed_runner_table_matches_the_ids`
/// pins that every row names a real [`Runner::id`].
pub const CHECK_RUNNERS: [(&str, &str); 15] = [
    ("cargo-test", "cargo test"),
    ("cargo-nextest", "cargo nextest run"),
    ("cargo-clippy", "cargo clippy"),
    ("cargo-fmt-check", "cargo fmt with a --check token"),
    ("cargo-check", "cargo check"),
    ("cargo-build", "cargo build"),
    ("just", "just <ci|test|lint|check...> recipe"),
    (
        "npm-script",
        "npm|pnpm|yarn test, or run <test|lint|build|check|typecheck|ci...>",
    ),
    ("vitest", "vitest"),
    ("playwright", "playwright test"),
    ("tsc", "tsc with a --noEmit token"),
    ("pytest", "pytest, python -m pytest"),
    ("go-test", "go test"),
    ("go-vet", "go vet"),
    (
        "gh-checks",
        "gh pr checks; gh run watch|view with --exit-status",
    ),
];

/// How a harness reports a command's exit status in its captured result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitSignal {
    /// `is_error` plus a leading `Exit code N`.
    Claude,
    /// `Process exited with code N`.
    Codex,
    /// No trustworthy exit signal in the adapter's output.
    None,
}

/// Closed table covering every entry of [`HARNESSES`] (pinned by
/// `harness_exit_table_covers_every_harness`).
pub const HARNESS_EXIT_SIGNAL: [(&str, ExitSignal); 6] = [
    ("claude", ExitSignal::Claude),
    ("codex", ExitSignal::Codex),
    ("opencode", ExitSignal::None),
    ("grok", ExitSignal::None),
    ("kimi", ExitSignal::None),
    ("omp", ExitSignal::None),
];

fn exit_signal(harness: &str) -> ExitSignal {
    HARNESS_EXIT_SIGNAL
        .iter()
        .find(|(h, _)| *h == harness)
        .map(|(_, s)| *s)
        .unwrap_or(ExitSignal::None)
}

// ---------------------------------------------------------------------------
// A. The command grammar
// ---------------------------------------------------------------------------

/// The separator FOLLOWING a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sep {
    And,
    Or,
    Pipe,
    Semi,
    Newline,
    Amp,
    End,
}

/// Split a shell command into `(segment, following separator)` pairs.
/// Separators count only outside quotes and at paren depth 0; a `&` that is
/// part of a redirection is not a separator; an unquoted heredoc swallows its
/// body into the current segment; a `#` at the start of a segment is a comment
/// up to the newline. Empty segments are dropped.
pub fn split_command(cmd: &str) -> Vec<(String, Sep)> {
    let chars: Vec<char> = cmd.chars().collect();
    let n = chars.len();
    let mut out: Vec<(String, Sep)> = Vec::new();
    let mut cur = String::new();
    let mut i = 0usize;
    let mut single = false;
    let mut double = false;
    let mut depth: i32 = 0;
    let mut pending: Vec<String> = Vec::new();

    fn emit(out: &mut Vec<(String, Sep)>, cur: &mut String, sep: Sep) {
        let t = cur.trim();
        if !t.is_empty() {
            out.push((t.to_string(), sep));
        }
        cur.clear();
    }

    while i < n {
        let c = chars[i];
        if single {
            cur.push(c);
            if c == '\'' {
                single = false;
            }
            i += 1;
            continue;
        }
        if double {
            cur.push(c);
            if c == '\\' && i + 1 < n {
                cur.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                double = false;
            }
            i += 1;
            continue;
        }
        if c == '\\' {
            cur.push(c);
            if i + 1 < n {
                cur.push(chars[i + 1]);
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if c == '\'' {
            single = true;
            cur.push(c);
            i += 1;
            continue;
        }
        if c == '"' {
            double = true;
            cur.push(c);
            i += 1;
            continue;
        }
        if c == '(' {
            depth += 1;
            cur.push(c);
            i += 1;
            continue;
        }
        if c == ')' {
            depth = (depth - 1).max(0);
            cur.push(c);
            i += 1;
            continue;
        }
        if depth > 0 {
            cur.push(c);
            i += 1;
            continue;
        }
        match c {
            '#' if cur.trim().is_empty() => {
                // A comment: skip to (not past) the newline.
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '<' if i + 1 < n
                && chars[i + 1] == '<'
                && !(i + 2 < n && chars[i + 2] == '<')
                && (i == 0 || chars[i - 1] != '<') =>
            {
                cur.push_str("<<");
                i += 2;
                if i < n && chars[i] == '-' {
                    cur.push('-');
                    i += 1;
                }
                while i < n && (chars[i] == ' ' || chars[i] == '\t') {
                    cur.push(chars[i]);
                    i += 1;
                }
                let mut word = String::new();
                let quote = if i < n && (chars[i] == '\'' || chars[i] == '"') {
                    let q = chars[i];
                    cur.push(q);
                    i += 1;
                    Some(q)
                } else {
                    None
                };
                while i < n {
                    let w = chars[i];
                    let stop = match quote {
                        Some(q) => w == q,
                        None => w.is_whitespace() || ";&|()<>".contains(w),
                    };
                    if stop {
                        break;
                    }
                    word.push(w);
                    cur.push(w);
                    i += 1;
                }
                if quote.is_some() && i < n {
                    cur.push(chars[i]);
                    i += 1;
                }
                if !word.is_empty() {
                    pending.push(word);
                }
            }
            '\n' => {
                if !pending.is_empty() {
                    let delims = std::mem::take(&mut pending);
                    for d in delims.iter() {
                        if i < n && chars[i] == '\n' {
                            cur.push('\n');
                            i += 1;
                        } else {
                            break;
                        }
                        loop {
                            let mut j = i;
                            while j < n && chars[j] != '\n' {
                                j += 1;
                            }
                            let line: String = chars[i..j].iter().collect();
                            cur.push_str(&line);
                            i = j;
                            if line.trim() == d.as_str() {
                                break;
                            }
                            if i < n {
                                cur.push('\n');
                                i += 1;
                            } else {
                                break;
                            }
                        }
                    }
                } else {
                    emit(&mut out, &mut cur, Sep::Newline);
                    i += 1;
                }
            }
            ';' => {
                emit(&mut out, &mut cur, Sep::Semi);
                i += 1;
            }
            '&' => {
                let prev = if i > 0 { chars[i - 1] } else { ' ' };
                let next = if i + 1 < n { chars[i + 1] } else { ' ' };
                if prev == '>' || prev == '<' || next == '>' {
                    // A redirection (`2>&1`, `>&2`, `<&`, `&>`, `&>>`).
                    cur.push(c);
                    i += 1;
                } else if next == '&' {
                    emit(&mut out, &mut cur, Sep::And);
                    i += 2;
                } else {
                    emit(&mut out, &mut cur, Sep::Amp);
                    i += 1;
                }
            }
            '|' => {
                let next = if i + 1 < n { chars[i + 1] } else { ' ' };
                if next == '|' {
                    emit(&mut out, &mut cur, Sep::Or);
                    i += 2;
                } else if next == '&' {
                    emit(&mut out, &mut cur, Sep::Pipe);
                    i += 2;
                } else {
                    emit(&mut out, &mut cur, Sep::Pipe);
                    i += 1;
                }
            }
            _ => {
                cur.push(c);
                i += 1;
            }
        }
    }
    emit(&mut out, &mut cur, Sep::End);
    out
}

/// Whitespace tokenizer that removes quotes (their content is kept) and
/// honours backslash escapes.
fn tokenize(seg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_tok = false;
    let mut single = false;
    let mut double = false;
    let mut it = seg.chars();
    while let Some(c) = it.next() {
        if single {
            if c == '\'' {
                single = false;
            } else {
                cur.push(c);
            }
            continue;
        }
        if double {
            if c == '"' {
                double = false;
            } else if c == '\\' {
                if let Some(nx) = it.next() {
                    cur.push(nx);
                }
            } else {
                cur.push(c);
            }
            continue;
        }
        match c {
            '\'' => {
                single = true;
                in_tok = true;
            }
            '"' => {
                double = true;
                in_tok = true;
            }
            '\\' => {
                in_tok = true;
                if let Some(nx) = it.next() {
                    cur.push(nx);
                }
            }
            c if c.is_whitespace() => {
                if in_tok {
                    out.push(std::mem::take(&mut cur));
                    in_tok = false;
                }
            }
            c => {
                cur.push(c);
                in_tok = true;
            }
        }
    }
    if in_tok {
        out.push(cur);
    }
    out
}

fn is_assignment(t: &str) -> bool {
    match t.find('=') {
        Some(p) if p > 0 => {
            let name = &t[..p];
            let mut cs = name.chars();
            let first_ok = cs
                .next()
                .map(|c| c.is_ascii_alphabetic() || c == '_')
                .unwrap_or(false);
            first_ok && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// If `toks[i]` starts a wrapper from the CLOSED list, the index just past the
/// wrapper and its own options; otherwise `None`.
fn skip_wrapper(toks: &[String], i: usize) -> Option<usize> {
    let t = toks.get(i)?.as_str();
    let at = |k: usize| toks.get(k).map(String::as_str);
    match t {
        "env" => {
            let mut k = i + 1;
            while let Some(x) = at(k) {
                if x == "-u" {
                    k += 2;
                } else if matches!(x, "-i" | "-" | "--")
                    || (x.starts_with("-u") && x.len() > 2)
                    || is_assignment(x)
                {
                    k += 1;
                } else {
                    break;
                }
            }
            Some(k)
        }
        "nice" => {
            let mut k = i + 1;
            while let Some(x) = at(k) {
                if x == "-n" {
                    k += 2;
                } else if x.starts_with("--adjustment=")
                    || (x.len() > 1
                        && x.starts_with('-')
                        && x[1..].chars().all(|c| c.is_ascii_digit()))
                    || (x.starts_with("-n") && x.len() > 2)
                {
                    k += 1;
                } else {
                    break;
                }
            }
            Some(k)
        }
        "ionice" => {
            let mut k = i + 1;
            while let Some(x) = at(k) {
                if x == "-c" || x == "-n" {
                    k += 2;
                } else if ((x.starts_with("-c") || x.starts_with("-n")) && x.len() > 2) || x == "-t"
                {
                    k += 1;
                } else {
                    break;
                }
            }
            Some(k)
        }
        "flock" => {
            let mut k = i + 1;
            while let Some(x) = at(k) {
                if !x.starts_with('-') {
                    break;
                }
                if matches!(
                    x,
                    "-w" | "-E" | "--timeout" | "--conflict-exit-code" | "--wait"
                ) {
                    k += 2;
                } else {
                    k += 1;
                }
            }
            // The lock path.
            if at(k).is_some() {
                k += 1;
            }
            Some(k)
        }
        "timeout" => {
            let mut k = i + 1;
            while let Some(x) = at(k) {
                if !x.starts_with('-') {
                    break;
                }
                if matches!(x, "-s" | "-k" | "--signal" | "--kill-after") {
                    k += 2;
                } else {
                    k += 1;
                }
            }
            // The duration.
            if at(k).is_some() {
                k += 1;
            }
            Some(k)
        }
        "time" | "command" | "exec" | "nohup" => Some(i + 1),
        "stdbuf" => {
            let mut k = i + 1;
            while let Some(x) = at(k) {
                if x.starts_with('-') {
                    k += 1;
                } else {
                    break;
                }
            }
            Some(k)
        }
        "npx" => {
            let mut k = i + 1;
            while let Some(x) = at(k) {
                if matches!(x, "-y" | "--yes" | "--no-install") {
                    k += 1;
                } else {
                    break;
                }
            }
            Some(k)
        }
        "pnpm" if matches!(at(i + 1), Some("exec") | Some("dlx")) => Some(i + 2),
        _ => None,
    }
}

/// Strip a CLOSED list of wrappers from the front of a segment and return the
/// remaining tokens: a leading `(`/`{`, `NAME=value` assignments, `env`,
/// `nice`, `ionice`, `flock`, `timeout`, `time`, `command`, `exec`, `nohup`,
/// `stdbuf`, `npx`, `pnpm exec|dlx`. Anything else stops the stripping.
pub fn strip_wrappers(segment: &str) -> Vec<String> {
    let mut s = segment.trim_start();
    let mut had_paren = false;
    loop {
        if let Some(r) = s.strip_prefix('(') {
            s = r.trim_start();
            had_paren = true;
        } else if let Some(r) = s.strip_prefix('{') {
            if r.starts_with(char::is_whitespace) {
                s = r.trim_start();
            } else {
                break;
            }
        } else {
            break;
        }
    }
    let mut toks = tokenize(s);
    if had_paren {
        if let Some(last) = toks.last_mut() {
            while last.ends_with(')') {
                last.pop();
            }
        }
        if toks.last().map(|l| l.is_empty()).unwrap_or(false) {
            toks.pop();
        }
    }
    let mut i = 0usize;
    while let Some(t) = toks.get(i) {
        if is_assignment(t) {
            i += 1;
            continue;
        }
        match skip_wrapper(&toks, i) {
            Some(k) => i = k.max(i + 1),
            None => break,
        }
    }
    toks.drain(..i.min(toks.len()));
    toks
}

fn script_name_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^(test|lint|build|check|typecheck|ci)[A-Za-z0-9:_-]*$").expect("valid regex")
    })
}

fn just_recipe_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^(ci|test|lint|check)[A-Za-z0-9_-]*$").expect("valid regex"))
}

/// Match the (already wrapper-stripped) tokens against the closed runner
/// table. Returns the runner and its label.
pub fn match_runner(toks: &[String]) -> Option<(Runner, String)> {
    let first = toks.first()?.as_str();
    let at = |k: usize| toks.get(k).map(String::as_str);
    let has = |needle: &str| toks.iter().any(|t| t == needle);
    match first {
        "cargo" => {
            let mut k = 1;
            if at(k).map(|t| t.starts_with('+')).unwrap_or(false) {
                k += 1;
            }
            match at(k)? {
                "test" => Some((Runner::CargoTest, "cargo test".into())),
                "nextest" if at(k + 1) == Some("run") => {
                    Some((Runner::CargoNextest, "cargo nextest".into()))
                }
                "clippy" => Some((Runner::CargoClippy, "cargo clippy".into())),
                "fmt" if has("--check") => {
                    Some((Runner::CargoFmtCheck, "cargo fmt --check".into()))
                }
                "check" => Some((Runner::CargoCheck, "cargo check".into())),
                "build" => Some((Runner::CargoBuild, "cargo build".into())),
                _ => None,
            }
        }
        "just" => {
            let mut k = 1;
            while let Some(x) = at(k) {
                if !x.starts_with('-') {
                    break;
                }
                if matches!(x, "-f" | "--justfile" | "-d" | "--working-directory") {
                    k += 2;
                } else {
                    k += 1;
                }
            }
            let recipe = at(k)?;
            if just_recipe_re().is_match(recipe) {
                Some((Runner::Just, format!("just {recipe}")))
            } else {
                None
            }
        }
        "npm" | "pnpm" | "yarn" => {
            let mut k = 1;
            while let Some(x) = at(k) {
                if !x.starts_with('-') {
                    break;
                }
                if matches!(
                    x,
                    "--prefix" | "-C" | "--workspace" | "-w" | "--cwd" | "--filter" | "-F"
                ) {
                    k += 2;
                } else {
                    k += 1;
                }
            }
            let sub = at(k)?;
            if sub == "test" || sub == "t" {
                return Some((Runner::NpmScript, format!("{first} test")));
            }
            if sub == "run" {
                let s = at(k + 1)?;
                if script_name_re().is_match(s) {
                    return Some((Runner::NpmScript, format!("{first} run {s}")));
                }
                return None;
            }
            if (first == "yarn" || first == "pnpm") && script_name_re().is_match(sub) {
                return Some((Runner::NpmScript, format!("{first} run {sub}")));
            }
            None
        }
        "vitest" => Some((Runner::Vitest, "vitest".into())),
        "playwright" if at(1) == Some("test") => {
            Some((Runner::Playwright, "playwright test".into()))
        }
        "tsc" if has("--noEmit") => Some((Runner::Tsc, "tsc --noEmit".into())),
        "pytest" => Some((Runner::Pytest, "pytest".into())),
        "python" | "python3" if at(1) == Some("-m") && at(2) == Some("pytest") => {
            Some((Runner::Pytest, "pytest".into()))
        }
        "go" => match at(1)? {
            "test" => Some((Runner::GoTest, "go test".into())),
            "vet" => Some((Runner::GoVet, "go vet".into())),
            _ => None,
        },
        "gh" => match (at(1), at(2)) {
            (Some("pr"), Some("checks")) => Some((Runner::GhChecks, "gh pr checks".into())),
            (Some("run"), Some("watch")) if has("--exit-status") => {
                Some((Runner::GhChecks, "gh run watch".into()))
            }
            (Some("run"), Some("view")) if has("--exit-status") => {
                Some((Runner::GhChecks, "gh run view".into()))
            }
            _ => None,
        },
        _ => None,
    }
}

const BENIGN: [&str; 12] = [
    "cd", "pushd", "popd", "export", "set", "unset", "true", ":", "echo", "printf", "sleep", "tail",
];

fn is_benign(toks: &[String]) -> bool {
    match toks.first() {
        None => true,
        Some(t) => t == "tee" || BENIGN.contains(&t.as_str()),
    }
}

fn is_errexit(toks: &[String]) -> bool {
    if toks.first().map(String::as_str) != Some("set") {
        return false;
    }
    let mut k = 1;
    while k < toks.len() {
        let t = toks[k].as_str();
        if t == "-o" && toks.get(k + 1).map(String::as_str) == Some("errexit") {
            return true;
        }
        if t.starts_with('-') && !t.starts_with("--") && t.len() > 1 && t[1..].contains('e') {
            return true;
        }
        k += 1;
    }
    false
}

fn secret_assign_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)=("[^"]*"|'[^']*'|[^\s"';&|]+)"#)
            .expect("valid regex")
    })
}

fn secret_name_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)token|secret|passw|key|auth|cred").expect("valid regex"))
}

/// Replace the value of every `NAME=value` whose NAME looks secret with `***`.
pub fn redact_secrets(s: &str) -> String {
    secret_assign_re()
        .replace_all(s, |c: &regex::Captures| {
            let name = &c[1];
            if secret_name_re().is_match(name) {
                format!("{name}=***")
            } else {
                c[0].to_string()
            }
        })
        .into_owned()
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What the closed grammar made of one Bash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    /// Every matched segment's runner, in order.
    pub runners: Vec<Runner>,
    /// Distinct labels joined with ` + `, capped.
    pub label: String,
    /// The (redacted, capped) command line.
    pub cmd: String,
    /// The command's exit status speaks for a PASS of its checks.
    pub pass_attributable: bool,
    /// ... and for a FAIL (no unrelated command could have failed).
    pub fail_attributable: bool,
    /// Why the exit status is not attributable (`None` when it is).
    pub why: Option<Why>,
}

/// Classify a Bash command: `None` unless at least one segment is a check.
pub fn classify_command(cmd: &str) -> Option<Classified> {
    let segs = split_command(cmd);
    let stripped: Vec<Vec<String>> = segs.iter().map(|(s, _)| strip_wrappers(s)).collect();
    let matches: Vec<Option<(Runner, String)>> = stripped.iter().map(|t| match_runner(t)).collect();
    let check_idx: Vec<usize> = matches
        .iter()
        .enumerate()
        .filter(|(_, m)| m.is_some())
        .map(|(i, _)| i)
        .collect();
    let first = *check_idx.first()?;

    let mut runners = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    for m in matches.iter().flatten() {
        runners.push(m.0);
        if !labels.contains(&m.1) {
            labels.push(m.1.clone());
        }
    }
    let label = truncate_chars_ellipsis(&labels.join(" + "), LABEL_CAP_CHARS);

    let pipefail = cmd.contains("pipefail");
    let mut why: Option<Why> = None;
    // A separator BEFORE the first check that can skip it.
    for (_, sep) in segs.iter().take(first) {
        if matches!(sep, Sep::Or | Sep::Amp | Sep::Pipe) && why.is_none() {
            why = Some(Why::Chained);
        }
    }
    let mut errexit_seen = (0..first).any(|j| is_errexit(&stripped[j]));
    for k in first..segs.len() {
        let sep = segs[k].1;
        let ok = match sep {
            Sep::And | Sep::End => true,
            Sep::Pipe => pipefail,
            Sep::Semi | Sep::Newline => errexit_seen,
            Sep::Or | Sep::Amp => false,
        };
        if !ok && why.is_none() {
            why = Some(if sep == Sep::Pipe {
                Why::Piped
            } else {
                Why::Chained
            });
        }
        if is_errexit(&stripped[k]) {
            errexit_seen = true;
        }
    }
    // `gh ... --json|--jq` exits 0 whatever the check states are.
    let gh_json = check_idx.iter().any(|&i| {
        matches[i].as_ref().map(|m| m.0) == Some(Runner::GhChecks)
            && stripped[i].iter().any(|t| {
                t == "--json" || t.starts_with("--json=") || t == "--jq" || t.starts_with("--jq=")
            })
    });
    if gh_json {
        why = Some(Why::Json);
    }
    let pass_attributable = why.is_none();
    let fail_attributable = pass_attributable
        && (0..segs.len())
            .filter(|j| !check_idx.contains(j))
            .all(|j| is_benign(&stripped[j]));

    let whole = redact_secrets(&collapse_ws(cmd));
    let cmd_out = if whole.chars().count() <= CMD_CAP_CHARS {
        whole
    } else {
        let joined = check_idx
            .iter()
            .map(|&i| redact_secrets(&collapse_ws(&segs[i].0)))
            .collect::<Vec<_>>()
            .join(" … ");
        truncate_chars_ellipsis(&joined, CMD_CAP_CHARS)
    };

    Some(Classified {
        runners,
        label,
        cmd: cmd_out,
        pass_attributable,
        fail_attributable,
        why,
    })
}

// --- output grammar --------------------------------------------------------

/// What a result's text says, runner by runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutV {
    Pass,
    Fail,
    Pending,
    NoMarker,
}

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new($pat).expect("valid regex"))
        }
    };
}

lazy_re!(
    re_cargo_test_fail,
    r"test result: FAILED|error: test failed|error: could not compile|error: \d+ targets? failed"
);
lazy_re!(re_cargo_test_pass, r"test result: ok\.");
lazy_re!(
    re_nextest_fail,
    r"error: test run failed|error: could not compile|\d+ tests? run: .*\d+ failed"
);
lazy_re!(re_nextest_pass, r"\d+ tests? run: \d+ passed");
lazy_re!(
    re_cargo_build_fail,
    r"(?m)error: could not compile|^error(\[E\d{4}\])?:"
);
lazy_re!(re_cargo_build_pass, r"(?m)^\s*Finished ");
lazy_re!(re_fmt_fail, r"(?m)^Diff in ");
lazy_re!(re_just_fail, r"error: Recipe `[^`]+` failed");
lazy_re!(
    re_vitest_fail,
    r"Test Files .*\d+ failed|Tests .*\d+ failed"
);
lazy_re!(re_vitest_pass, r"Test Files\s+\d+ passed");
lazy_re!(re_pw_fail, r"(?m)^\s*\d+ failed");
lazy_re!(re_pw_pass, r"(?m)^\s*\d+ passed");
lazy_re!(re_pytest_fail, r"=+ .*\d+ (failed|errors?)\b.*=+");
lazy_re!(re_pytest_pass, r"=+ .*\d+ passed.*=+");
lazy_re!(re_go_fail, r"(?m)^(FAIL|--- FAIL)");
lazy_re!(re_go_pass, r"(?m)^(ok\s|PASS$)");

/// `(fail marker present, pass marker present, runner has a pass column)`.
fn runner_marks(r: Runner, text: &str) -> (bool, bool, bool) {
    match r {
        Runner::CargoTest => (
            re_cargo_test_fail().is_match(text),
            re_cargo_test_pass().is_match(text),
            true,
        ),
        Runner::CargoNextest => (
            re_nextest_fail().is_match(text),
            re_nextest_pass().is_match(text),
            true,
        ),
        Runner::CargoClippy | Runner::CargoCheck | Runner::CargoBuild => (
            re_cargo_build_fail().is_match(text),
            re_cargo_build_pass().is_match(text),
            true,
        ),
        Runner::CargoFmtCheck => (re_fmt_fail().is_match(text), false, false),
        Runner::Just => (re_just_fail().is_match(text), false, false),
        Runner::NpmScript | Runner::Tsc => (false, false, false),
        Runner::Vitest => (
            re_vitest_fail().is_match(text),
            re_vitest_pass().is_match(text),
            true,
        ),
        Runner::Playwright => (
            re_pw_fail().is_match(text),
            re_pw_pass().is_match(text),
            true,
        ),
        Runner::Pytest => (
            re_pytest_fail().is_match(text),
            re_pytest_pass().is_match(text),
            true,
        ),
        Runner::GoTest | Runner::GoVet => (
            re_go_fail().is_match(text),
            re_go_pass().is_match(text),
            true,
        ),
        Runner::GhChecks => (false, false, false),
    }
}

/// gh-checks: the WHOLE output must parse as a JSON array of objects with a
/// string `bucket`.
fn gh_checks_verdict(text: &str) -> Option<OutV> {
    let v: Value = serde_json::from_str(text.trim()).ok()?;
    let arr = v.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut buckets = Vec::new();
    for o in arr {
        buckets.push(o.get("bucket")?.as_str()?.to_string());
    }
    if buckets.iter().any(|b| b == "fail" || b == "cancel") {
        Some(OutV::Fail)
    } else if buckets.iter().any(|b| b == "pending") {
        Some(OutV::Pending)
    } else {
        Some(OutV::Pass)
    }
}

/// The output-grammar verdict for a check's result text. FAIL markers beat
/// PASS markers; a PASS needs a positive marker for every runner that has a
/// pass column (and at least one such runner).
pub fn output_verdict(runners: &[Runner], text: &str) -> OutV {
    let mut any_fail = false;
    let mut pending = false;
    let mut need_pass = 0usize;
    let mut got_pass = 0usize;
    for &r in runners {
        if r == Runner::GhChecks {
            need_pass += 1;
            match gh_checks_verdict(text) {
                Some(OutV::Fail) => any_fail = true,
                Some(OutV::Pending) => pending = true,
                Some(OutV::Pass) => got_pass += 1,
                _ => {}
            }
            continue;
        }
        let (f, p, has_pass) = runner_marks(r, text);
        if f {
            any_fail = true;
        }
        if has_pass {
            need_pass += 1;
            if p {
                got_pass += 1;
            }
        }
    }
    if any_fail {
        OutV::Fail
    } else if pending {
        OutV::Pending
    } else if need_pass > 0 && got_pass == need_pass {
        OutV::Pass
    } else {
        OutV::NoMarker
    }
}

// ---------------------------------------------------------------------------
// B. Tracking and outcome
// ---------------------------------------------------------------------------

/// How a background run ended, from its `<task-notification>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notification {
    Completed(Option<i32>),
    Failed(Option<i32>),
    Stopped,
}

/// The captured result of one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultInfo {
    pub is_error: bool,
    /// A leading `Exit code N` (Claude Code).
    pub exit_claude: Option<i32>,
    /// `exited with code N` (codex).
    pub exit_codex: Option<i32>,
    pub interrupted: bool,
    pub out: OutV,
}

/// One check run found in the transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub seq: u32,
    pub at: Option<i64>,
    pub tool_use_id: Option<String>,
    pub runners: Vec<Runner>,
    pub label: String,
    pub cmd: String,
    pub pass_attributable: bool,
    pub fail_attributable: bool,
    pub why: Option<Why>,
    pub result: Option<ResultInfo>,
    pub background_task_id: Option<String>,
    pub notification: Option<Notification>,
}

/// One counted edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRec {
    pub seq: u32,
    pub at: Option<i64>,
    pub path: String,
}

/// What the tracker leaves on a [`super::SessionActivity`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckRuns {
    pub runs: Vec<CheckRun>,
    pub edits: Vec<EditRec>,
    pub edits_ignored: u32,
    /// Seqs of `git push` Bash calls whose result was not an error.
    pub pushes: Vec<u32>,
}

#[derive(Debug, Clone, Default)]
struct Pend {
    run: Option<usize>,
    edit: Option<usize>,
    push: Option<u32>,
}

#[derive(Debug, Clone)]
struct EditTrack {
    rec: EditRec,
    failed: bool,
}

/// Folds a transcript's tool calls and results into [`CheckRuns`]. Three
/// calls from the parse loop: [`Tracker::on_tool_use`],
/// [`Tracker::on_user_event`], [`Tracker::finish`].
#[derive(Debug, Clone, Default)]
pub struct Tracker {
    runs: Vec<CheckRun>,
    edits: Vec<EditTrack>,
    edits_ignored: u32,
    pushes: Vec<u32>,
    pending: BTreeMap<String, Pend>,
}

fn is_sidechain(v: &Value) -> bool {
    v.get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Agent bookkeeping and scratch paths: edits there are not "the code".
fn is_ignored_path(p: &str) -> bool {
    ["/.claude/", "/.codex/", "/.kimi/", "/.omp/", "/.grok/"]
        .iter()
        .any(|m| p.contains(m))
        || p.starts_with("/tmp/")
        || p.starts_with("/var/tmp/")
}

lazy_re!(re_exit_claude, r"\A\s*Exit code (\d+)");
lazy_re!(re_exit_codex, r"exited with code (\d+)");
lazy_re!(re_exit_summary, r"exit code (\d+)");

fn parse_code(re: &Regex, text: &str) -> Option<i32> {
    re.captures(text)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<i32>().ok())
}

fn tag_text<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let a = s.find(&open)? + open.len();
    let b = s[a..].find(&close)? + a;
    Some(s[a..b].trim())
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// An assistant `tool_use` block. `seq` is the running tool-call count
    /// (`act.tool_calls` right after its increment).
    pub fn on_tool_use(&mut self, v: &Value, block: &Value, seq: u32) {
        if is_sidechain(v) {
            return;
        }
        let name = block.get("name").and_then(Value::as_str).unwrap_or("");
        let at = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso_utc);
        let mut pend = Pend::default();
        match name {
            "Bash" => {
                let Some(cmd) = block.pointer("/input/command").and_then(Value::as_str) else {
                    return;
                };
                let id = block.get("id").and_then(Value::as_str).map(str::to_string);
                if let Some(c) = classify_command(cmd) {
                    self.runs.push(CheckRun {
                        seq,
                        at,
                        tool_use_id: id,
                        runners: c.runners,
                        label: c.label,
                        cmd: c.cmd,
                        pass_attributable: c.pass_attributable,
                        fail_attributable: c.fail_attributable,
                        why: c.why,
                        result: None,
                        background_task_id: None,
                        notification: None,
                    });
                    pend.run = Some(self.runs.len() - 1);
                }
                if super::git_action_of(cmd).iter().any(|(k, _)| k == "push") {
                    pend.push = Some(seq);
                }
            }
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
                let path = block
                    .pointer("/input/file_path")
                    .or_else(|| block.pointer("/input/notebook_path"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if is_ignored_path(path) {
                    self.edits_ignored += 1;
                } else {
                    self.edits.push(EditTrack {
                        rec: EditRec {
                            seq,
                            at,
                            path: path.to_string(),
                        },
                        failed: false,
                    });
                    pend.edit = Some(self.edits.len() - 1);
                }
            }
            _ => {}
        }
        if pend.run.is_some() || pend.edit.is_some() || pend.push.is_some() {
            if let Some(id) = block.get("id").and_then(Value::as_str) {
                self.pending.insert(id.to_string(), pend);
            }
        }
    }

    /// A `role:"user"` record: pairs `tool_result` blocks with their calls and
    /// reads `<task-notification>` completions of background runs.
    pub fn on_user_event(&mut self, v: &Value) {
        if is_sidechain(v) {
            return;
        }
        if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
            let is_result =
                |b: &Value| b.get("type").and_then(Value::as_str) == Some("tool_result");
            let n = blocks.iter().filter(|b| is_result(b)).count();
            let event_tul = if n == 1 { v.get("toolUseResult") } else { None };
            for b in blocks.iter().filter(|b| is_result(b)) {
                self.apply_result(b, event_tul);
            }
        }
        if let Some(text) = super::user_message_text(v) {
            if text.contains("<task-notification>") {
                self.apply_notifications(&text);
            }
        }
    }

    fn apply_result(&mut self, b: &Value, event_tul: Option<&Value>) {
        let Some(id) = b.get("tool_use_id").and_then(Value::as_str) else {
            return;
        };
        let Some(p) = self.pending.remove(id) else {
            return;
        };
        let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        if let Some(e) = p.edit {
            if is_error {
                self.edits[e].failed = true;
            }
        }
        if let Some(s) = p.push {
            if !is_error {
                self.pushes.push(s);
            }
        }
        if let Some(r) = p.run {
            let text = super::tool_result_text(b);
            let interrupted = event_tul
                .and_then(|t| t.get("interrupted"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let bg = event_tul
                .and_then(|t| t.get("backgroundTaskId"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let run = &mut self.runs[r];
            let out = output_verdict(&run.runners, &text);
            run.result = Some(ResultInfo {
                is_error,
                exit_claude: parse_code(re_exit_claude(), &text),
                exit_codex: parse_code(re_exit_codex(), &text),
                interrupted,
                out,
            });
            run.background_task_id = bg;
        }
    }

    fn apply_notifications(&mut self, text: &str) {
        for chunk in text.split("<task-notification>").skip(1) {
            let chunk = chunk.split("</task-notification>").next().unwrap_or(chunk);
            let status = tag_text(chunk, "status").unwrap_or("");
            let summary = tag_text(chunk, "summary").unwrap_or("");
            let code = parse_code(re_exit_summary(), summary);
            let note = match status {
                "completed" => Notification::Completed(code),
                "failed" => Notification::Failed(code),
                "killed" | "stopped" => Notification::Stopped,
                _ => continue,
            };
            let by_id = tag_text(chunk, "tool-use-id").filter(|s| !s.is_empty());
            let by_task = tag_text(chunk, "task-id").filter(|s| !s.is_empty());
            let idx = by_id
                .and_then(|id| {
                    self.runs
                        .iter()
                        .position(|r| r.tool_use_id.as_deref() == Some(id))
                })
                .or_else(|| {
                    by_task.and_then(|t| {
                        self.runs
                            .iter()
                            .position(|r| r.background_task_id.as_deref() == Some(t))
                    })
                });
            if let Some(i) = idx {
                self.runs[i].notification = Some(note);
            }
        }
    }

    /// The collected runs and counted edits (an edit whose result was an error
    /// does not count; an edit with no result still does).
    pub fn finish(self) -> CheckRuns {
        CheckRuns {
            runs: self.runs,
            edits: self
                .edits
                .into_iter()
                .filter(|e| !e.failed)
                .map(|e| e.rec)
                .collect(),
            edits_ignored: self.edits_ignored,
            pushes: self.pushes,
        }
    }
}

/// The outcome of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub outcome: Outcome,
    pub basis: Option<Basis>,
    pub why: Option<Why>,
    pub exit_code: Option<i32>,
}

fn unknown(why: Why) -> Resolved {
    Resolved {
        outcome: Outcome::Unknown,
        basis: None,
        why: Some(why),
        exit_code: None,
    }
}

/// A decisive exit status, if the command's exit speaks for its checks;
/// otherwise the `why` to carry on.
fn exit_decision(run: &CheckRun, code: i32, basis: Basis) -> Result<Resolved, Why> {
    let carry = run.why.unwrap_or(Why::Chained);
    if code == 0 {
        if run.pass_attributable {
            return Ok(Resolved {
                outcome: Outcome::Pass,
                basis: Some(basis),
                why: None,
                exit_code: Some(0),
            });
        }
        return Err(carry);
    }
    if run.fail_attributable {
        if code == 8 && run.runners.iter().all(|r| *r == Runner::GhChecks) {
            return Ok(Resolved {
                outcome: Outcome::Unknown,
                basis: None,
                why: Some(Why::Pending),
                exit_code: Some(8),
            });
        }
        return Ok(Resolved {
            outcome: Outcome::Fail,
            basis: Some(basis),
            why: None,
            exit_code: Some(code),
        });
    }
    Err(carry)
}

/// The outcome ladder (first match wins).
pub fn resolve(run: &CheckRun, harness: &str) -> Resolved {
    // 1) nothing came back.
    if run.result.is_none() && run.notification.is_none() {
        return unknown(Why::NoResult);
    }
    // 2) interrupted.
    if run.result.as_ref().map(|r| r.interrupted).unwrap_or(false) {
        return unknown(Why::Interrupted);
    }
    // 3) a background run, settled by its notification.
    if run.background_task_id.is_some() || run.notification.is_some() {
        return match run.notification {
            Some(Notification::Completed(code)) => {
                exit_decision(run, code.unwrap_or(0), Basis::Notification).unwrap_or_else(unknown)
            }
            Some(Notification::Failed(Some(n))) => {
                exit_decision(run, n, Basis::Notification).unwrap_or_else(unknown)
            }
            Some(Notification::Failed(None)) => {
                if run.fail_attributable {
                    Resolved {
                        outcome: Outcome::Fail,
                        basis: Some(Basis::Notification),
                        why: None,
                        exit_code: None,
                    }
                } else {
                    unknown(run.why.unwrap_or(Why::Chained))
                }
            }
            Some(Notification::Stopped) => unknown(Why::BackgroundStopped),
            None => unknown(Why::Background),
        };
    }
    let Some(res) = run.result.as_ref() else {
        return unknown(Why::NoResult);
    };
    // 4) the harness's exit tier.
    let carry: Why;
    match exit_signal(harness) {
        ExitSignal::Claude => {
            let code = if !res.is_error {
                Some(0)
            } else {
                res.exit_claude
            };
            match code {
                None => return unknown(Why::NoExitCode),
                Some(c) => match exit_decision(run, c, Basis::Exit) {
                    Ok(r) => return r,
                    Err(w) => carry = w,
                },
            }
        }
        ExitSignal::Codex => match res.exit_codex {
            None => return unknown(Why::NoExitCode),
            Some(c) => match exit_decision(run, c, Basis::Exit) {
                Ok(r) => return r,
                Err(w) => carry = w,
            },
        },
        ExitSignal::None => carry = Why::NoExitSignal,
    }
    // 5) the output grammar.
    match res.out {
        OutV::Pass => Resolved {
            outcome: Outcome::Pass,
            basis: Some(Basis::Output),
            why: None,
            exit_code: None,
        },
        OutV::Fail => Resolved {
            outcome: Outcome::Fail,
            basis: Some(Basis::Output),
            why: None,
            exit_code: None,
        },
        OutV::Pending => unknown(Why::Pending),
        OutV::NoMarker => unknown(carry),
    }
}

// ---------------------------------------------------------------------------
// C. Evidence, verdict, copy
// ---------------------------------------------------------------------------

/// One persisted check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCheck {
    pub seq: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
    pub runners: Vec<String>,
    pub label: String,
    pub cmd: String,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis: Option<Basis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<Why>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// What `sessions.evidence_json` (V0046) holds. Internal column, versioned by
/// `v` (deliberately no `SCHEMA` string constant).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEvidence {
    pub v: u32,
    /// The NEWEST [`MAX_CHECKS_KEPT`] checks, oldest first.
    pub checks: Vec<SessionCheck>,
    pub checks_total: u32,
    pub edit_count: u32,
    pub edits_ignored: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_edit_seq: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_edit_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_push_after_edit_seq: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unnamed_edits: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub unnamed_edits_total: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn basename(p: &str) -> &str {
    p.rsplit(['/', '\\']).next().unwrap_or(p)
}

/// Resolve every run and fold the tracker's output into the persisted shape.
pub fn build_evidence(
    runs: &CheckRuns,
    harness: &str,
    closing_text: Option<&str>,
) -> SessionEvidence {
    let all: Vec<SessionCheck> = runs
        .runs
        .iter()
        .map(|r| {
            let res = resolve(r, harness);
            SessionCheck {
                seq: r.seq,
                at: r.at,
                runners: r.runners.iter().map(|x| x.id().to_string()).collect(),
                label: r.label.clone(),
                cmd: r.cmd.clone(),
                outcome: res.outcome,
                basis: res.basis,
                why: res.why,
                exit_code: res.exit_code,
            }
        })
        .collect();
    let checks_total = all.len() as u32;
    let skip = all.len().saturating_sub(MAX_CHECKS_KEPT);
    let checks: Vec<SessionCheck> = all.into_iter().skip(skip).collect();

    let last_edit = runs.edits.iter().max_by_key(|e| e.seq);
    let last_edit_seq = last_edit.map(|e| e.seq);
    let first_push_after_edit_seq =
        last_edit_seq.and_then(|ls| runs.pushes.iter().copied().filter(|p| *p > ls).min());

    let mut unnamed: Vec<String> = Vec::new();
    if let Some(closing) = closing_text {
        for e in &runs.edits {
            let b = basename(&e.path);
            if b.is_empty() || closing.contains(b) || unnamed.iter().any(|u| u == b) {
                continue;
            }
            unnamed.push(b.to_string());
        }
    }
    let unnamed_edits_total = unnamed.len() as u32;
    unnamed.truncate(MAX_UNNAMED_EDITS);

    SessionEvidence {
        v: EVIDENCE_VERSION,
        checks,
        checks_total,
        edit_count: runs.edits.len() as u32,
        edits_ignored: runs.edits_ignored,
        last_edit_seq,
        last_edit_at: last_edit.and_then(|e| e.at),
        first_push_after_edit_seq,
        unnamed_edits: unnamed,
        unnamed_edits_total,
    }
}

/// Parse a persisted column. `None` for garbage or a version this build does
/// not know.
pub fn parse_evidence(json: &str) -> Option<SessionEvidence> {
    let ev: SessionEvidence = serde_json::from_str(json).ok()?;
    (ev.v == EVIDENCE_VERSION).then_some(ev)
}

/// The derived read-time verdict. Pure: no clock, no row access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSummary {
    pub verdict: Verdict,
    pub phrase: Option<String>,
    pub deciding: Option<SessionCheck>,
    pub checks_after_edit: u32,
}

/// Is `c` after the last edit? A gh-checks-only check additionally needs a
/// push between the edit and itself: CI checks the PUSHED head.
pub fn is_after_last_edit(ev: &SessionEvidence, c: &SessionCheck) -> bool {
    let Some(ls) = ev.last_edit_seq else {
        return false;
    };
    if c.seq <= ls {
        return false;
    }
    let gh_only = !c.runners.is_empty() && c.runners.iter().all(|r| r == "gh-checks");
    if gh_only {
        return ev
            .first_push_after_edit_seq
            .map(|p| p < c.seq)
            .unwrap_or(false);
    }
    true
}

fn outcome_word(o: Outcome) -> &'static str {
    match o {
        Outcome::Pass => "passed",
        Outcome::Fail => "failed",
        Outcome::Unknown => "outcome unknown",
    }
}

fn suffix_parts(c: &SessionCheck, more: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    if c.outcome == Outcome::Fail {
        if let Some(n) = c.exit_code {
            parts.push(format!("exit {n}"));
        }
    }
    if c.basis == Some(Basis::Output) {
        parts.push("read from its output".to_string());
    }
    if more > 0 {
        parts.push(format!("+{more} more"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(", "))
    }
}

/// The per-check sentence both the CLI and the SPA print.
pub fn check_text(c: &SessionCheck) -> String {
    let mut s = match c.outcome {
        Outcome::Pass => "passed".to_string(),
        Outcome::Fail => "failed".to_string(),
        Outcome::Unknown => match c.why {
            Some(w) => format!("outcome unknown: {}", w.words()),
            None => "outcome unknown".to_string(),
        },
    };
    let mut parts: Vec<String> = Vec::new();
    if c.outcome == Outcome::Fail {
        if let Some(n) = c.exit_code {
            parts.push(format!("exit {n}"));
        }
    }
    if c.basis == Some(Basis::Output) {
        parts.push("read from its output".to_string());
    }
    if !parts.is_empty() {
        s.push_str(&format!(" ({})", parts.join(", ")));
    }
    if c.basis == Some(Basis::Notification) {
        s.push_str(" (background run)");
    }
    s
}

/// Derive the verdict and its phrase from persisted evidence.
pub fn summarize(ev: &SessionEvidence) -> EvidenceSummary {
    let none = |verdict| EvidenceSummary {
        verdict,
        phrase: None,
        deciding: None,
        checks_after_edit: 0,
    };
    if ev.last_edit_seq.is_none() {
        return none(Verdict::NoEdits);
    }
    let after: Vec<&SessionCheck> = ev
        .checks
        .iter()
        .filter(|c| is_after_last_edit(ev, c))
        .collect();
    let checks_after_edit = after.len() as u32;
    if after.is_empty() {
        let before = ev.checks.iter().rev().find(|c| !is_after_last_edit(ev, c));
        return match before {
            Some(c) => EvidenceSummary {
                verdict: Verdict::Stale,
                phrase: Some(format!(
                    "edited after the last check: {} {}",
                    c.label,
                    outcome_word(c.outcome)
                )),
                deciding: Some(c.clone()),
                checks_after_edit: 0,
            },
            None => EvidenceSummary {
                verdict: Verdict::Unchecked,
                phrase: Some("edited, no checks ran".to_string()),
                deciding: None,
                checks_after_edit: 0,
            },
        };
    }
    // Per label, the latest DECISIVE after-edit run.
    let mut by_label: Vec<(&str, &SessionCheck)> = Vec::new();
    for c in after.iter().copied() {
        if c.outcome == Outcome::Unknown {
            continue;
        }
        match by_label.iter_mut().find(|(l, _)| *l == c.label.as_str()) {
            Some(slot) => slot.1 = c,
            None => by_label.push((c.label.as_str(), c)),
        }
    }
    if by_label.is_empty() {
        let last = after[after.len() - 1];
        let mut phrase = format!("{} after the last edit: outcome unknown", last.label);
        if let Some(w) = last.why {
            phrase.push_str(&format!(" ({})", w.words()));
        }
        return EvidenceSummary {
            verdict: Verdict::Unconfirmed,
            phrase: Some(phrase),
            deciding: Some(last.clone()),
            checks_after_edit,
        };
    }
    let failing = by_label
        .iter()
        .filter(|(_, c)| c.outcome == Outcome::Fail)
        .map(|(_, c)| *c)
        .max_by_key(|c| c.seq);
    let more = by_label.len() - 1;
    if let Some(f) = failing {
        return EvidenceSummary {
            verdict: Verdict::Failing,
            phrase: Some(format!(
                "{} failed after the last edit{}",
                f.label,
                suffix_parts(f, more)
            )),
            deciding: Some(f.clone()),
            checks_after_edit,
        };
    }
    let best = by_label
        .iter()
        .map(|(_, c)| *c)
        .max_by_key(|c| c.seq)
        .expect("non-empty");
    EvidenceSummary {
        verdict: Verdict::Verified,
        phrase: Some(format!(
            "{} passed after the last edit{}",
            best.label,
            suffix_parts(best, more)
        )),
        deciding: Some(best.clone()),
        checks_after_edit,
    }
}

/// The verdict alone for evidence that is absent: used by readers for a NULL
/// or unreadable column.
pub fn not_extracted() -> EvidenceSummary {
    EvidenceSummary {
        verdict: Verdict::NotExtracted,
        phrase: None,
        deciding: None,
        checks_after_edit: 0,
    }
}

// keep the harness list referenced so a new harness is a compile-visible
// decision for HARNESS_EXIT_SIGNAL's coverage test.
const _: usize = HARNESSES.len();

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::parse_session_activity;
    use serde_json::json;

    fn sp(cmd: &str) -> Vec<(String, Sep)> {
        split_command(cmd)
    }

    fn pairs(v: Vec<(&str, Sep)>) -> Vec<(String, Sep)> {
        v.into_iter().map(|(s, p)| (s.to_string(), p)).collect()
    }

    #[test]
    fn split_command_golden() {
        use Sep::*;
        assert_eq!(sp("cargo test"), pairs(vec![("cargo test", End)]));
        assert_eq!(sp("a && b"), pairs(vec![("a", And), ("b", End)]));
        assert_eq!(sp("a || b"), pairs(vec![("a", Or), ("b", End)]));
        assert_eq!(sp("a | b"), pairs(vec![("a", Pipe), ("b", End)]));
        assert_eq!(sp("a |& b"), pairs(vec![("a", Pipe), ("b", End)]));
        assert_eq!(sp("a; b"), pairs(vec![("a", Semi), ("b", End)]));
        assert_eq!(sp("a\nb"), pairs(vec![("a", Newline), ("b", End)]));
        assert_eq!(sp("a & b"), pairs(vec![("a", Amp), ("b", End)]));
        // A redirection's `&` is not a separator.
        assert_eq!(
            sp("cargo test 2>&1 | tail -30"),
            pairs(vec![("cargo test 2>&1", Pipe), ("tail -30", End)])
        );
        assert_eq!(sp("a >&2 && b"), pairs(vec![("a >&2", And), ("b", End)]));
        assert_eq!(
            sp("cmd &> /dev/null && x"),
            pairs(vec![("cmd &> /dev/null", And), ("x", End)])
        );
        // Quoted separators do not split.
        assert_eq!(
            sp("echo 'a && b' && ls"),
            pairs(vec![("echo 'a && b'", And), ("ls", End)])
        );
        assert_eq!(sp("echo \"a | b\""), pairs(vec![("echo \"a | b\"", End)]));
        // Parens: separators inside `$(...)` and `(...)` do not split.
        assert_eq!(
            sp("x=$(a && b) && c"),
            pairs(vec![("x=$(a && b)", And), ("c", End)])
        );
        assert_eq!(sp("(a && b)"), pairs(vec![("(a && b)", End)]));
        // A heredoc body, even one that says `cargo test`, stays in its segment.
        assert_eq!(
            sp("cat <<EOF\ncargo test\nEOF\nls"),
            pairs(vec![("cat <<EOF\ncargo test\nEOF", Newline), ("ls", End)])
        );
        assert_eq!(
            sp("cat <<'EOF' && x\nignored\nEOF"),
            pairs(vec![("cat <<'EOF'", And), ("x\nignored\nEOF", End)])
        );
        // A comment line is dropped.
        assert_eq!(
            sp("# run tests\ncargo test"),
            pairs(vec![("cargo test", End)])
        );
        // A dangling separator leaves no empty segment.
        assert_eq!(sp("a &&"), pairs(vec![("a", And)]));
    }

    fn toks(s: &str) -> Vec<String> {
        strip_wrappers(s)
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strip_wrappers_golden() {
        assert_eq!(
            toks("flock /tmp/x.lock env CARGO_BUILD_JOBS=2 nice -n 20 ionice -c 3 cargo test -p kb-core --profile fast"),
            strs(&["cargo", "test", "-p", "kb-core", "--profile", "fast"])
        );
        assert_eq!(toks("FOO=bar cargo test"), strs(&["cargo", "test"]));
        assert_eq!(
            toks("ENV_TOKEN=\"a b\" cargo test"),
            strs(&["cargo", "test"])
        );
        assert_eq!(toks("env -i FOO=1 cargo build"), strs(&["cargo", "build"]));
        assert_eq!(toks("env -u HOME cargo build"), strs(&["cargo", "build"]));
        assert_eq!(toks("nice -10 cargo test"), strs(&["cargo", "test"]));
        assert_eq!(toks("ionice -c3 cargo test"), strs(&["cargo", "test"]));
        assert_eq!(
            toks("flock -w 5 /tmp/l cargo test"),
            strs(&["cargo", "test"])
        );
        assert_eq!(toks("timeout 60 cargo test"), strs(&["cargo", "test"]));
        assert_eq!(
            toks("timeout -s KILL 60 cargo test"),
            strs(&["cargo", "test"])
        );
        assert_eq!(toks("time cargo test"), strs(&["cargo", "test"]));
        assert_eq!(toks("nohup cargo test"), strs(&["cargo", "test"]));
        assert_eq!(toks("stdbuf -oL cargo test"), strs(&["cargo", "test"]));
        assert_eq!(toks("npx -y vitest run"), strs(&["vitest", "run"]));
        assert_eq!(toks("pnpm exec vitest"), strs(&["vitest"]));
        assert_eq!(toks("pnpm dlx tsc --noEmit"), strs(&["tsc", "--noEmit"]));
        assert_eq!(toks("(cargo test)"), strs(&["cargo", "test"]));
        // Anything outside the closed list stops the stripping.
        assert_eq!(toks("sudo cargo test"), strs(&["sudo", "cargo", "test"]));
    }

    fn label_of(cmd: &str) -> Option<String> {
        classify_command(cmd).map(|c| c.label)
    }

    #[test]
    fn classify_command_golden() {
        let yes: &[(&str, &str)] = &[
            ("cargo test", "cargo test"),
            ("cargo test -p kb-core --profile fast", "cargo test"),
            (
                "flock /tmp/x.lock env CARGO_BUILD_JOBS=2 nice -n 20 ionice -c 3 cargo test -p kb-core --profile fast",
                "cargo test",
            ),
            ("cd x && cargo test", "cargo test"),
            ("cargo +nightly test", "cargo test"),
            ("cargo nextest run", "cargo nextest"),
            ("cargo clippy --workspace -- -D warnings", "cargo clippy"),
            ("cargo fmt --all -- --check", "cargo fmt --check"),
            ("cargo check -p x", "cargo check"),
            ("cargo build --release", "cargo build"),
            ("just ci-spa", "just ci-spa"),
            ("just -f j test-all", "just test-all"),
            ("npm test", "npm test"),
            ("npm run lint", "npm run lint"),
            ("pnpm build", "pnpm run build"),
            ("yarn typecheck", "yarn run typecheck"),
            ("yarn test", "yarn test"),
            ("npm --prefix web run test:unit", "npm run test:unit"),
            ("vitest run", "vitest"),
            ("npx vitest run web/src/x.test.ts", "vitest"),
            ("playwright test", "playwright test"),
            ("npx playwright test --project=chromium", "playwright test"),
            ("tsc --noEmit", "tsc --noEmit"),
            ("npx tsc -p x --noEmit", "tsc --noEmit"),
            ("pytest -q", "pytest"),
            ("python3 -m pytest", "pytest"),
            ("go test ./...", "go test"),
            ("go vet ./...", "go vet"),
            ("gh pr checks 12", "gh pr checks"),
            ("gh run watch 5 --exit-status", "gh run watch"),
            ("gh run view 1 --exit-status", "gh run view"),
            ("# run tests\ncargo test", "cargo test"),
            ("cargo test && cargo test", "cargo test"),
            (
                "cargo fmt --all -- --check && cargo clippy -- -D warnings && cargo test",
                "cargo fmt --check + cargo clippy + cargo test",
            ),
        ];
        assert!(yes.len() >= 30, "need at least 30 golden cases");
        for (cmd, want) in yes {
            assert_eq!(label_of(cmd).as_deref(), Some(*want), "cmd: {cmd}");
        }
        let no: &[&str] = &[
            "cargo fmt --all",
            "just deploy",
            "npm install",
            "pnpm install",
            "tsc",
            "go build ./...",
            "gh run view 123",
            "bash -c \"cargo test\"",
            "echo \"cargo test\"",
            "git commit -m \"make cargo test pass\"",
            "cat <<EOF\ncargo test\nEOF",
            "git status",
            "cargo doc",
            "cargo run",
            "cargo nextest list",
            "sudo cargo test",
        ];
        for cmd in no {
            assert_eq!(label_of(cmd), None, "cmd: {cmd}");
        }
        // One check row, three runners.
        let c = classify_command(
            "cargo fmt --all -- --check && cargo clippy -- -D warnings && cargo test",
        )
        .unwrap();
        assert_eq!(
            c.runners,
            vec![
                Runner::CargoFmtCheck,
                Runner::CargoClippy,
                Runner::CargoTest
            ]
        );
        let dup = classify_command("cargo test && cargo test").unwrap();
        assert_eq!(dup.runners.len(), 2);
    }

    fn attr(cmd: &str) -> (bool, bool, Option<Why>) {
        let c = classify_command(cmd).expect("a check");
        (c.pass_attributable, c.fail_attributable, c.why)
    }

    #[test]
    fn attributability_golden() {
        assert_eq!(attr("cargo test"), (true, true, None));
        assert_eq!(attr("cd web && npm test"), (true, true, None));
        assert_eq!(
            attr("cargo test 2>&1 | tail -30"),
            (false, false, Some(Why::Piped))
        );
        assert_eq!(
            attr("set -o pipefail; cargo test | tail"),
            (true, true, None)
        );
        assert_eq!(
            attr("cargo test || true"),
            (false, false, Some(Why::Chained))
        );
        assert_eq!(
            attr("cargo test; echo ok"),
            (false, false, Some(Why::Chained))
        );
        assert_eq!(
            attr("cargo test & wait"),
            (false, false, Some(Why::Chained))
        );
        assert_eq!(attr("set -e; cargo test; echo ok"), (true, true, None));
        assert_eq!(
            attr("set -euo pipefail\ncargo test\necho ok"),
            (true, true, None)
        );
        // `git pull && cargo test` exiting 1 is NOT a cargo test failure.
        assert_eq!(attr("git pull && cargo test"), (true, false, None));
        assert_eq!(
            attr("echo hi | cargo test"),
            (false, false, Some(Why::Chained))
        );
        // gh with --json exits 0 whatever the states are.
        assert_eq!(
            attr("gh pr checks 1 --json bucket"),
            (false, false, Some(Why::Json))
        );
        assert_eq!(
            attr("gh pr checks 1 --jq '.[]'"),
            (false, false, Some(Why::Json))
        );
        assert_eq!(attr("gh pr checks 1"), (true, true, None));
    }

    fn ov(r: &[Runner], t: &str) -> OutV {
        output_verdict(r, t)
    }

    #[test]
    fn output_grammar_golden() {
        use OutV::*;
        let t = Runner::CargoTest;
        assert_eq!(ov(&[t], "test result: ok. 3 passed; 0 failed"), Pass);
        assert_eq!(ov(&[t], "test result: FAILED. 1 passed; 1 failed"), Fail);
        assert_eq!(ov(&[t], "running 0 tests"), NoMarker);
        assert_eq!(
            ov(
                &[t],
                "test result: ok. 1 passed\nerror: test failed, to rerun"
            ),
            Fail
        );
        let n = Runner::CargoNextest;
        assert_eq!(
            ov(&[n], "Summary [ 1.2s] 5 tests run: 5 passed, 0 skipped"),
            Pass
        );
        assert_eq!(
            ov(&[n], "Summary [ 1.2s] 5 tests run: 3 passed, 2 failed"),
            Fail
        );
        assert_eq!(ov(&[n], "nothing"), NoMarker);
        let c = Runner::CargoClippy;
        assert_eq!(
            ov(
                &[c],
                "    Checking x v0.1\n    Finished `dev` profile in 2s"
            ),
            Pass
        );
        assert_eq!(ov(&[c], "error[E0308]: mismatched types"), Fail);
        assert_eq!(ov(&[c], "error: could not compile `x`"), Fail);
        assert_eq!(ov(&[c], "warning: unused variable"), NoMarker);
        let f = Runner::CargoFmtCheck;
        assert_eq!(ov(&[f], "Diff in /x/y.rs:1:"), Fail);
        assert_eq!(ov(&[f], ""), NoMarker);
        let j = Runner::Just;
        assert_eq!(
            ov(&[j], "error: Recipe `ci` failed on line 3 with exit code 1"),
            Fail
        );
        assert_eq!(ov(&[j], "all good"), NoMarker);
        let v = Runner::Vitest;
        assert_eq!(
            ov(
                &[v],
                " Test Files  3 passed (3)\n      Tests  10 passed (10)"
            ),
            Pass
        );
        assert_eq!(ov(&[v], " Test Files  1 failed | 2 passed (3)"), Fail);
        assert_eq!(ov(&[v], "no summary"), NoMarker);
        let p = Runner::Playwright;
        assert_eq!(ov(&[p], "  3 passed (4s)"), Pass);
        assert_eq!(ov(&[p], "  1 failed\n  2 passed (4s)"), Fail);
        assert_eq!(ov(&[p], "Running 3 tests"), NoMarker);
        let y = Runner::Pytest;
        assert_eq!(ov(&[y], "===== 3 passed in 0.12s ====="), Pass);
        assert_eq!(ov(&[y], "===== 1 failed, 2 passed in 0.12s ====="), Fail);
        assert_eq!(ov(&[y], "collected 0 items"), NoMarker);
        let g = Runner::GoTest;
        assert_eq!(ov(&[g], "ok  \tgithub.com/x/y\t0.01s"), Pass);
        assert_eq!(ov(&[g], "FAIL\tgithub.com/x/y\t0.01s"), Fail);
        assert_eq!(ov(&[g], "?   \tpkg [no test files]"), NoMarker);
        let h = Runner::GhChecks;
        assert_eq!(ov(&[h], r#"[{"bucket":"pass","name":"a"}]"#), Pass);
        assert_eq!(ov(&[h], r#"[{"bucket":"pass"},{"bucket":"fail"}]"#), Fail);
        assert_eq!(ov(&[h], r#"[{"bucket":"pass"},{"bucket":"cancel"}]"#), Fail);
        assert_eq!(ov(&[h], r#"[{"bucket":"pending"}]"#), Pending);
        assert_eq!(ov(&[h], "not json"), NoMarker);
        assert_eq!(ov(&[h], "[]"), NoMarker);
        assert_eq!(ov(&[Runner::NpmScript], "test result: ok."), NoMarker);
        assert_eq!(ov(&[Runner::Tsc], "Found 0 errors"), NoMarker);
        // Multi-runner: a PASS needs a pass marker for every runner that has one.
        assert_eq!(ov(&[c, t], "Finished `dev` profile"), NoMarker);
        assert_eq!(
            ov(&[c, t], "Finished `dev` profile\ntest result: ok. 1 passed"),
            Pass
        );
        assert_eq!(
            ov(
                &[c, t],
                "Finished `dev` profile\ntest result: FAILED. 0 passed"
            ),
            Fail
        );
    }

    // --- tracking through the real parse loop ---------------------------------

    fn jl(events: &[serde_json::Value]) -> String {
        events
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn tu(id: &str, name: &str, input: serde_json::Value) -> serde_json::Value {
        json!({"type":"tool_use","id":id,"name":name,"input":input})
    }

    fn asst(ts: &str, blocks: Vec<serde_json::Value>) -> serde_json::Value {
        json!({"type":"assistant","timestamp":ts,
               "message":{"role":"assistant","content":blocks}})
    }

    fn asst_side(ts: &str, blocks: Vec<serde_json::Value>) -> serde_json::Value {
        json!({"type":"assistant","timestamp":ts,"isSidechain":true,
               "message":{"role":"assistant","content":blocks}})
    }

    fn res(id: &str, content: &str, is_error: bool) -> serde_json::Value {
        json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"content":content,"is_error":is_error}]}})
    }

    #[test]
    fn edit_tracking() {
        let ev = vec![
            asst(
                "2026-06-10T09:00:00Z",
                vec![tu("e1", "Edit", json!({"file_path":"/w/a.rs"}))],
            ),
            res("e1", "ok", false),
            asst(
                "2026-06-10T09:00:10Z",
                vec![tu("e2", "Edit", json!({"file_path":"/w/b.rs"}))],
            ),
            // A failed Edit is not counted.
            res("e2", "String to replace not found", true),
            // A bookkeeping path does not count (and is counted as ignored).
            asst(
                "2026-06-10T09:00:20Z",
                vec![tu(
                    "e3",
                    "Write",
                    json!({"file_path":"/home/u/.claude/projects/x/memory.md"}),
                )],
            ),
            res("e3", "ok", false),
            // A relative path counts; an edit with no result still counts.
            asst(
                "2026-06-10T09:00:30Z",
                vec![tu("e4", "Edit", json!({"file_path":"src/rel.rs"}))],
            ),
            // A sidechain edit is ignored entirely.
            asst_side(
                "2026-06-10T09:00:40Z",
                vec![tu("e5", "Edit", json!({"file_path":"/w/side.rs"}))],
            ),
            asst(
                "2026-06-10T09:00:50Z",
                vec![tu("e6", "Write", json!({"file_path":"/tmp/scratch.txt"}))],
            ),
        ];
        let act = parse_session_activity(&jl(&ev));
        let runs = act.check_runs;
        let paths: Vec<&str> = runs.edits.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["/w/a.rs", "src/rel.rs"]);
        assert_eq!(
            runs.edits.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert_eq!(runs.edits_ignored, 2);
        assert!(runs.edits[0].at.is_some());
    }

    #[test]
    fn push_counts_only_when_its_result_is_not_an_error() {
        let ev = vec![
            asst(
                "2026-06-10T09:00:00Z",
                vec![tu("p1", "Bash", json!({"command":"git push origin main"}))],
            ),
            res("p1", "rejected", true),
            asst(
                "2026-06-10T09:00:10Z",
                vec![tu("p2", "Bash", json!({"command":"git push origin main"}))],
            ),
            res("p2", "To github.com:x/y.git", false),
        ];
        let act = parse_session_activity(&jl(&ev));
        assert_eq!(act.check_runs.pushes, vec![2]);
    }

    #[test]
    fn sidechain_checks_are_ignored() {
        let ev = vec![
            asst_side(
                "2026-06-10T09:00:00Z",
                vec![tu("c1", "Bash", json!({"command":"cargo test"}))],
            ),
            asst(
                "2026-06-10T09:00:10Z",
                vec![tu("c2", "Bash", json!({"command":"cargo test"}))],
            ),
        ];
        let act = parse_session_activity(&jl(&ev));
        assert_eq!(act.check_runs.runs.len(), 1);
        assert_eq!(act.check_runs.runs[0].seq, 2);
    }

    #[test]
    fn secret_env_values_redacted() {
        let c = classify_command("API_TOKEN=abc123 cargo test").unwrap();
        assert_eq!(c.cmd, "API_TOKEN=*** cargo test");
        let c = classify_command("DB_PASSWORD=\"a b\" RUST_LOG=debug cargo test").unwrap();
        assert_eq!(c.cmd, "DB_PASSWORD=*** RUST_LOG=debug cargo test");
        assert!(!c.cmd.contains("a b"));
        assert_eq!(redact_secrets("MY_KEY=zzz"), "MY_KEY=***");
        assert_eq!(redact_secrets("PLAIN=zzz"), "PLAIN=zzz");
    }

    #[test]
    fn cmd_cap_keeps_check_segments() {
        let long = format!("echo {} && cargo test -p widget", "x".repeat(300));
        let c = classify_command(&long).unwrap();
        assert_eq!(c.cmd, "cargo test -p widget");
        assert!(c.cmd.chars().count() <= CMD_CAP_CHARS);

        let huge = format!("cargo test {}", "y".repeat(400));
        let c = classify_command(&huge).unwrap();
        assert!(c.cmd.chars().count() <= CMD_CAP_CHARS);
        assert!(c.cmd.ends_with('…'));

        // Whitespace is collapsed when the whole command fits.
        let c = classify_command("cargo   test\n  -p   widget").unwrap();
        assert_eq!(c.cmd, "cargo test -p widget");
    }

    // --- the outcome ladder ----------------------------------------------------

    fn base_run(pa: bool, fa: bool, why: Option<Why>) -> CheckRun {
        CheckRun {
            seq: 1,
            at: None,
            tool_use_id: Some("t".into()),
            runners: vec![Runner::CargoTest],
            label: "cargo test".into(),
            cmd: "cargo test".into(),
            pass_attributable: pa,
            fail_attributable: fa,
            why,
            result: None,
            background_task_id: None,
            notification: None,
        }
    }

    fn with_result(
        mut r: CheckRun,
        is_error: bool,
        claude: Option<i32>,
        codex: Option<i32>,
        out: OutV,
    ) -> CheckRun {
        r.result = Some(ResultInfo {
            is_error,
            exit_claude: claude,
            exit_codex: codex,
            interrupted: false,
            out,
        });
        r
    }

    fn rs(r: &CheckRun, h: &str) -> (Outcome, Option<Basis>, Option<Why>, Option<i32>) {
        let x = resolve(r, h);
        (x.outcome, x.basis, x.why, x.exit_code)
    }

    #[test]
    fn outcome_ladder() {
        use Outcome::*;
        // 1) nothing came back.
        assert_eq!(
            rs(&base_run(true, true, None), "claude"),
            (Unknown, None, Some(Why::NoResult), None)
        );
        // 2) interrupted.
        let mut r = with_result(
            base_run(true, true, None),
            false,
            None,
            None,
            OutV::NoMarker,
        );
        r.result.as_mut().unwrap().interrupted = true;
        assert_eq!(
            rs(&r, "claude"),
            (Unknown, None, Some(Why::Interrupted), None)
        );

        // 4) claude: the exit tier.
        let ok = with_result(
            base_run(true, true, None),
            false,
            None,
            None,
            OutV::NoMarker,
        );
        assert_eq!(rs(&ok, "claude"), (Pass, Some(Basis::Exit), None, Some(0)));
        let bad = with_result(
            base_run(true, true, None),
            true,
            Some(101),
            None,
            OutV::NoMarker,
        );
        assert_eq!(
            rs(&bad, "claude"),
            (Fail, Some(Basis::Exit), None, Some(101))
        );
        let denied = with_result(base_run(true, true, None), true, None, None, OutV::NoMarker);
        assert_eq!(
            rs(&denied, "claude"),
            (Unknown, None, Some(Why::NoExitCode), None)
        );
        // Piped: the exit speaks for `tail`, so the output grammar decides.
        let piped_pass = with_result(
            base_run(false, false, Some(Why::Piped)),
            false,
            None,
            None,
            OutV::Pass,
        );
        assert_eq!(
            rs(&piped_pass, "claude"),
            (Pass, Some(Basis::Output), None, None)
        );
        let piped_none = with_result(
            base_run(false, false, Some(Why::Piped)),
            false,
            None,
            None,
            OutV::NoMarker,
        );
        assert_eq!(
            rs(&piped_none, "claude"),
            (Unknown, None, Some(Why::Piped), None)
        );
        // A non-zero exit that is not FAIL-attributable falls to the output.
        let mixed = with_result(base_run(true, false, None), true, Some(1), None, OutV::Fail);
        assert_eq!(
            rs(&mixed, "claude"),
            (Fail, Some(Basis::Output), None, None)
        );
        let mixed_none = with_result(
            base_run(true, false, None),
            true,
            Some(1),
            None,
            OutV::NoMarker,
        );
        assert_eq!(
            rs(&mixed_none, "claude"),
            (Unknown, None, Some(Why::Chained), None)
        );

        // 4) codex.
        let cx0 = with_result(
            base_run(true, true, None),
            false,
            None,
            Some(0),
            OutV::NoMarker,
        );
        assert_eq!(rs(&cx0, "codex"), (Pass, Some(Basis::Exit), None, Some(0)));
        let cx1 = with_result(
            base_run(true, true, None),
            true,
            None,
            Some(2),
            OutV::NoMarker,
        );
        assert_eq!(rs(&cx1, "codex"), (Fail, Some(Basis::Exit), None, Some(2)));
        let cxn = with_result(base_run(true, true, None), false, None, None, OutV::Pass);
        assert_eq!(
            rs(&cxn, "codex"),
            (Unknown, None, Some(Why::NoExitCode), None)
        );

        // The four no-signal harnesses fall to the output grammar.
        for h in ["opencode", "grok", "kimi", "omp"] {
            let p = with_result(base_run(true, true, None), false, None, None, OutV::Pass);
            assert_eq!(rs(&p, h), (Pass, Some(Basis::Output), None, None), "{h}");
            let f = with_result(base_run(true, true, None), false, None, None, OutV::Fail);
            assert_eq!(rs(&f, h), (Fail, Some(Basis::Output), None, None), "{h}");
            let n = with_result(
                base_run(true, true, None),
                false,
                None,
                None,
                OutV::NoMarker,
            );
            assert_eq!(
                rs(&n, h),
                (Unknown, None, Some(Why::NoExitSignal), None),
                "{h}"
            );
        }

        // gh-checks exit 8 = checks still pending.
        let mut gh = with_result(
            base_run(true, true, None),
            true,
            None,
            Some(8),
            OutV::NoMarker,
        );
        gh.runners = vec![Runner::GhChecks];
        assert_eq!(
            rs(&gh, "codex"),
            (Unknown, None, Some(Why::Pending), Some(8))
        );
        let mut gh_out = with_result(base_run(true, true, None), false, None, None, OutV::Pending);
        gh_out.runners = vec![Runner::GhChecks];
        assert_eq!(
            rs(&gh_out, "grok"),
            (Unknown, None, Some(Why::Pending), None)
        );

        // 3) background runs.
        let bg = |n: Option<Notification>, pa: bool| {
            let mut r = with_result(
                base_run(pa, pa, if pa { None } else { Some(Why::Piped) }),
                false,
                None,
                None,
                OutV::NoMarker,
            );
            r.background_task_id = Some("bg1".into());
            r.notification = n;
            r
        };
        assert_eq!(
            rs(&bg(Some(Notification::Completed(None)), true), "claude"),
            (Pass, Some(Basis::Notification), None, Some(0))
        );
        assert_eq!(
            rs(&bg(Some(Notification::Completed(Some(0))), true), "claude"),
            (Pass, Some(Basis::Notification), None, Some(0))
        );
        assert_eq!(
            rs(&bg(Some(Notification::Failed(Some(2))), true), "claude"),
            (Fail, Some(Basis::Notification), None, Some(2))
        );
        assert_eq!(
            rs(&bg(Some(Notification::Failed(None)), true), "claude"),
            (Fail, Some(Basis::Notification), None, None)
        );
        assert_eq!(
            rs(&bg(Some(Notification::Stopped), true), "claude"),
            (Unknown, None, Some(Why::BackgroundStopped), None)
        );
        assert_eq!(
            rs(&bg(None, true), "claude"),
            (Unknown, None, Some(Why::Background), None)
        );
        // A completed background run whose exit is not attributable stays unknown.
        assert_eq!(
            rs(&bg(Some(Notification::Completed(Some(0))), false), "claude"),
            (Unknown, None, Some(Why::Piped), None)
        );
    }

    #[test]
    fn harness_exit_table_covers_every_harness() {
        for h in HARNESSES {
            assert!(
                HARNESS_EXIT_SIGNAL.iter().any(|(n, _)| *n == h),
                "harness {h} has no exit-signal entry"
            );
        }
        assert_eq!(HARNESS_EXIT_SIGNAL.len(), HARNESSES.len());
        assert_eq!(exit_signal("claude"), ExitSignal::Claude);
        assert_eq!(exit_signal("codex"), ExitSignal::Codex);
        assert_eq!(exit_signal("something-new"), ExitSignal::None);
    }

    #[test]
    fn closed_runner_table_matches_the_ids() {
        let all = [
            Runner::CargoTest,
            Runner::CargoNextest,
            Runner::CargoClippy,
            Runner::CargoFmtCheck,
            Runner::CargoCheck,
            Runner::CargoBuild,
            Runner::Just,
            Runner::NpmScript,
            Runner::Vitest,
            Runner::Playwright,
            Runner::Tsc,
            Runner::Pytest,
            Runner::GoTest,
            Runner::GoVet,
            Runner::GhChecks,
        ];
        assert_eq!(all.len(), CHECK_RUNNERS.len());
        for r in all {
            assert!(
                CHECK_RUNNERS.iter().any(|(id, _)| *id == r.id()),
                "{} missing from CHECK_RUNNERS",
                r.id()
            );
        }
    }

    // --- evidence, verdict, copy ----------------------------------------------

    fn runs_of(edits: &[(u32, &str)], checks: usize) -> CheckRuns {
        let mut r = CheckRuns::default();
        for (s, p) in edits {
            r.edits.push(EditRec {
                seq: *s,
                at: None,
                path: p.to_string(),
            });
        }
        for i in 0..checks {
            let mut c = with_result(
                base_run(true, true, None),
                false,
                None,
                None,
                OutV::NoMarker,
            );
            c.seq = (i + 1) as u32;
            r.runs.push(c);
        }
        r
    }

    #[test]
    fn cap_keeps_newest_16_and_counts_total() {
        let ev = build_evidence(&runs_of(&[], 20), "claude", None);
        assert_eq!(ev.checks.len(), MAX_CHECKS_KEPT);
        assert_eq!(ev.checks_total, 20);
        assert_eq!(ev.checks.first().unwrap().seq, 5);
        assert_eq!(ev.checks.last().unwrap().seq, 20);
        assert_eq!(ev.v, EVIDENCE_VERSION);
    }

    #[test]
    fn unnamed_edits_basename_containment() {
        let runs = runs_of(&[(1, "/w/a/b.rs"), (2, "/w/c/d.ts"), (3, "/w/e/b.rs")], 0);
        let ev = build_evidence(&runs, "claude", Some("I changed b.rs only"));
        assert_eq!(ev.unnamed_edits, vec!["d.ts".to_string()]);
        assert_eq!(ev.unnamed_edits_total, 1);
        // No closing text: nothing can be called unnamed.
        let none = build_evidence(&runs, "claude", None);
        assert!(none.unnamed_edits.is_empty());
        assert_eq!(none.unnamed_edits_total, 0);
        // Capped at 12, with the total kept.
        let many: Vec<(u32, String)> = (0..15).map(|i| (i + 1, format!("/w/f{i}.rs"))).collect();
        let refs: Vec<(u32, &str)> = many.iter().map(|(s, p)| (*s, p.as_str())).collect();
        let ev = build_evidence(&runs_of(&refs, 0), "claude", Some("nothing named"));
        assert_eq!(ev.unnamed_edits.len(), MAX_UNNAMED_EDITS);
        assert_eq!(ev.unnamed_edits_total, 15);
        // Case-sensitive containment.
        let ev = build_evidence(
            &runs_of(&[(1, "/w/B.rs")], 0),
            "claude",
            Some("changed b.rs"),
        );
        assert_eq!(ev.unnamed_edits, vec!["B.rs".to_string()]);
    }

    #[test]
    fn evidence_json_round_trips_and_rejects_unknown_versions() {
        let runs = runs_of(&[(1, "/w/a.rs")], 2);
        let ev = build_evidence(&runs, "claude", Some("done"));
        let s = serde_json::to_string(&ev).unwrap();
        assert_eq!(parse_evidence(&s), Some(ev.clone()));
        let mut v: serde_json::Value = serde_json::from_str(&s).unwrap();
        v["v"] = json!(2);
        assert_eq!(parse_evidence(&v.to_string()), None);
        assert_eq!(parse_evidence("not json"), None);
        assert_eq!(parse_evidence("{}"), None);
    }

    fn ck(seq: u32, label: &str, outcome: Outcome) -> SessionCheck {
        SessionCheck {
            seq,
            at: None,
            runners: vec!["cargo-test".into()],
            label: label.into(),
            cmd: format!("$ {label}"),
            outcome,
            basis: if outcome == Outcome::Unknown {
                None
            } else {
                Some(Basis::Exit)
            },
            why: if outcome == Outcome::Unknown {
                Some(Why::Piped)
            } else {
                None
            },
            exit_code: match outcome {
                Outcome::Pass => Some(0),
                Outcome::Fail => Some(1),
                Outcome::Unknown => None,
            },
        }
    }

    fn evd(
        last_edit: Option<u32>,
        push: Option<u32>,
        checks: Vec<SessionCheck>,
    ) -> SessionEvidence {
        SessionEvidence {
            v: 1,
            checks_total: checks.len() as u32,
            checks,
            edit_count: u32::from(last_edit.is_some()),
            edits_ignored: 0,
            last_edit_seq: last_edit,
            last_edit_at: None,
            first_push_after_edit_seq: push,
            unnamed_edits: vec![],
            unnamed_edits_total: 0,
        }
    }

    fn sum(e: &SessionEvidence) -> (Verdict, Option<String>) {
        let s = summarize(e);
        (s.verdict, s.phrase)
    }

    #[test]
    fn verdict_golden() {
        use Outcome::*;
        use Verdict::*;
        let t = "cargo test";
        let c = "cargo clippy";
        // edit -> pass
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(7, t, Pass)])),
            (
                Verified,
                Some("cargo test passed after the last edit".into())
            )
        );
        // pass -> edit
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(3, t, Pass)])),
            (
                Stale,
                Some("edited after the last check: cargo test passed".into())
            )
        );
        // fail -> edit
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(3, t, Fail)])),
            (
                Stale,
                Some("edited after the last check: cargo test failed".into())
            )
        );
        // edit only
        assert_eq!(
            sum(&evd(Some(5), None, vec![])),
            (Unchecked, Some("edited, no checks ran".into()))
        );
        // edit -> fail with exit 101
        let mut f = ck(7, t, Fail);
        f.exit_code = Some(101);
        assert_eq!(
            sum(&evd(Some(5), None, vec![f])),
            (
                Failing,
                Some("cargo test failed after the last edit (exit 101)".into())
            )
        );
        // edit -> piped-unknown only
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(7, t, Unknown)])),
            (
                Unconfirmed,
                Some("cargo test after the last edit: outcome unknown (exit status hidden by a pipe)".into())
            )
        );
        // clippy fails, test passes
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(7, c, Fail), ck(8, t, Pass)])),
            (
                Failing,
                Some("cargo clippy failed after the last edit (exit 1, +1 more)".into())
            )
        );
        // flaky: fail then pass on the same label -> verified
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(6, t, Fail), ck(8, t, Pass)])),
            (
                Verified,
                Some("cargo test passed after the last edit".into())
            )
        );
        // pass then fail on the same label -> failing
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(6, t, Pass), ck(8, t, Fail)])).0,
            Failing
        );
        // output-basis pass
        let mut o = ck(7, t, Pass);
        o.basis = Some(Basis::Output);
        o.exit_code = None;
        assert_eq!(
            sum(&evd(Some(5), None, vec![o])),
            (
                Verified,
                Some("cargo test passed after the last edit (read from its output)".into())
            )
        );
        // two labels pass
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(7, t, Pass), ck(8, c, Pass)])),
            (
                Verified,
                Some("cargo clippy passed after the last edit (+1 more)".into())
            )
        );
        // an unknown run beside a decisive one on another label
        assert_eq!(
            sum(&evd(Some(5), None, vec![ck(7, t, Unknown), ck(8, c, Pass)])),
            (
                Verified,
                Some("cargo clippy passed after the last edit".into())
            )
        );
        // gh after the edit with NO push in between counts as before
        let mut gh = ck(7, "gh pr checks", Pass);
        gh.runners = vec!["gh-checks".into()];
        assert_eq!(
            sum(&evd(Some(5), None, vec![gh.clone()])),
            (
                Stale,
                Some("edited after the last check: gh pr checks passed".into())
            )
        );
        // ... and with a push between, it verifies
        assert_eq!(
            sum(&evd(Some(5), Some(6), vec![gh.clone()])),
            (
                Verified,
                Some("gh pr checks passed after the last edit".into())
            )
        );
        // a push AFTER the check does not help
        assert_eq!(sum(&evd(Some(5), Some(9), vec![gh])).0, Stale);
        // no edits
        assert_eq!(sum(&evd(None, None, vec![ck(1, t, Pass)])), (NoEdits, None));
        // a bookkeeping-only edit: the tracker counted none
        let bookkeeping = CheckRuns {
            edits_ignored: 1,
            ..CheckRuns::default()
        };
        let ev = build_evidence(&bookkeeping, "claude", Some("done"));
        assert_eq!(ev.edits_ignored, 1);
        assert_eq!(sum(&ev), (NoEdits, None));
        assert_eq!(not_extracted().verdict, NotExtracted);
    }

    #[test]
    fn copy_golden() {
        let mut c = ck(1, "cargo test", Outcome::Pass);
        assert_eq!(check_text(&c), "passed");
        c.outcome = Outcome::Fail;
        c.exit_code = Some(101);
        assert_eq!(check_text(&c), "failed (exit 101)");
        c.exit_code = None;
        c.basis = Some(Basis::Output);
        assert_eq!(check_text(&c), "failed (read from its output)");
        let mut p = ck(1, "cargo test", Outcome::Pass);
        p.basis = Some(Basis::Output);
        assert_eq!(check_text(&p), "passed (read from its output)");
        p.basis = Some(Basis::Notification);
        assert_eq!(check_text(&p), "passed (background run)");
        let mut f = ck(1, "cargo test", Outcome::Fail);
        f.basis = Some(Basis::Notification);
        f.exit_code = Some(2);
        assert_eq!(check_text(&f), "failed (exit 2) (background run)");
        let u = ck(1, "cargo test", Outcome::Unknown);
        assert_eq!(
            check_text(&u),
            "outcome unknown: exit status hidden by a pipe"
        );
        let words = [
            (Why::Piped, "exit status hidden by a pipe"),
            (Why::Chained, "exit status mixed with other commands"),
            (Why::Json, "--json output carries no status"),
            (Why::Pending, "checks still pending"),
            (Why::NoExitSignal, "this harness records no exit status"),
            (Why::NoExitCode, "ended in an error without an exit code"),
            (Why::NoResult, "no result recorded"),
            (Why::Background, "ran in the background, no completion seen"),
            (Why::BackgroundStopped, "background run was stopped"),
            (Why::Interrupted, "interrupted"),
        ];
        for (w, s) in words {
            assert_eq!(w.words(), s);
        }
        assert_eq!(
            serde_json::to_string(&Verdict::NoEdits).unwrap(),
            "\"no_edits\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::NotExtracted).unwrap(),
            "\"not_extracted\""
        );
    }

    // invariant:11 the verdict is surfaced, never scored
    #[test]
    fn no_ranking_module_reads_session_evidence() {
        const RANKERS: &[(&str, &str)] = &[
            ("memory.rs", include_str!("../memory.rs")),
            ("fusion.rs", include_str!("../fusion.rs")),
            ("resurface.rs", include_str!("../resurface.rs")),
            ("triage.rs", include_str!("../triage.rs")),
            ("query.rs", include_str!("../query.rs")),
            ("sessions/projects.rs", include_str!("projects.rs")),
        ];
        for (name, src) in RANKERS {
            for needle in ["checks::", "evidence_json", "EvidenceSummary"] {
                assert!(
                    !src.contains(needle),
                    "{name} names `{needle}` -- session evidence is SURFACED, NEVER SCORED \
                     (V0046's own header): it may be rendered beside a row, never folded \
                     into a rank, a sort key or a filter"
                );
            }
        }
    }
}
