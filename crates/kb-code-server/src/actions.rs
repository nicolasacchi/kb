//! V71-E2 — `GET /api/actions`: `kbc-actions/1`, the typed dispatch table
//! over "the thing under the pointer" (D5).
//!
//! The research report's thesis, which this module implements literally:
//! *the action menu is not UI, it is a typed dispatch table over a resolved
//! target*. Every code tool builds a different menu per SURFACE (right-click
//! in the diff, in the file, in the search results → three unrelated lists);
//! kb-code already has a deterministic, LLM-free notion of what a thing IS,
//! so the list can be server-rendered ONCE and consumed identically by the
//! SPA menu (right-click · `.` · `Shift+F10` · the ContextMenu key · the
//! drag-select pill · the mobile sheet) and by `kb-code act`.
//!
//! # Four rules this route exists to keep
//!
//! 1. **Ordering is stable per target kind.** JetBrains' `Alt+Enter` reorders
//!    under the user, which is why nobody can muscle-memorise it (the
//!    quick-fix plugin author's canonical indictment). Here the order is a
//!    `const` table, pinned by `golden_action_table_is_stable`; a browser-
//!    local "recent" section is allowed ABOVE it, visually separate, and is
//!    never merged into these groups.
//! 2. **Mutating rows are ABSENT for a bearer caller, not disabled.** The
//!    verdict is the SERVER's ([`kb_server::middleware::is_loopback_origin`]
//!    + `[review] remote_mutations`), never a client guess — risk 9 of the
//!    selection-actions report. The response SAYS the group was withheld and
//!    why ([`ActionsOut::mutations`]); that is a property of the CALLER, not
//!    a confirmation that any particular mutating route exists (the same
//!    thing `GET /api/repos`'s `loopback` bool already reports).
//! 3. **"Ask here" on every target kind.** D5 makes it a REQUIRED row for
//!    every kind — widening the shipped question channel — and
//!    `every_target_kind_offers_ask_here` fails the build if a kind ever
//!    ships without it.
//! 4. **Candidate/likely never auto-navigate** (D5, risk 5). This route
//!    deliberately does NOT run the resolve ladder (it must open in <50 ms
//!    from already-warm data), so it cannot know a definition's trust class
//!    — and a row that cannot PROVE `exact` must not auto-navigate. Every
//!    row therefore ships `auto_navigate: false`, and the peek rung is
//!    ordered FIRST for a symbol. The reader's own `gd`, which does resolve,
//!    keeps its single-exact-match rule unchanged.
//!
//! # Targets, not modes
//!
//! `resolve_targets` returns an ORDERED list (Embark's model): the segmented
//! control in the menu header is that list, so "is the user pointing at a
//! symbol or at a range?" is not a hidden cycle and not a mode — it is a
//! visible list with a default the gesture implies. The kind vocabulary is
//! CLOSED at five names (`symbol` · `range` · `text` · `path` · `enclosing`),
//! exactly the segments D5 names.
//!
//! # Not here
//!
//! Nothing is persisted, nothing is cached, no LLM (root CLAUDE.md #2's
//! posture). No plumber rule table (`[plumb]`, W1 of the report) — the three
//! built-in shapes are enough to earn the `path` target and a user-authored
//! table is a config surface with its own golden fixtures. No `review`/
//! `finding`/`diff-range`/`canvas-card` surface targets, no act-all over a
//! set, no two-target actions: each needs a surface this unit does not
//! touch, and a target kind with no rows is the dead surface this whole
//! milestone is about.

use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

pub const ACTIONS_SCHEMA: &str = "kbc-actions/1";

/// Longest selected text this route will echo back as a `text` target or
/// search query. A selection larger than this is still a `range` target —
/// only the TEXT interpretation is dropped, with a note.
pub const MAX_TEXT_TARGET_BYTES: usize = 512;

// --- targets --------------------------------------------------------------

/// The CLOSED target vocabulary — D5's segmented control, one name per
/// segment. A sixth name is a wire change, not an implementation detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Symbol,
    Range,
    Text,
    Path,
    Enclosing,
}

impl TargetKind {
    pub const ALL: &'static [TargetKind] = &[
        TargetKind::Symbol,
        TargetKind::Range,
        TargetKind::Text,
        TargetKind::Path,
        TargetKind::Enclosing,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::Symbol => "symbol",
            TargetKind::Range => "range",
            TargetKind::Text => "text",
            TargetKind::Path => "path",
            TargetKind::Enclosing => "enclosing",
        }
    }
}

/// One resolved thing the pointer is on. Every optional field is absent
/// rather than defaulted — a target never claims a line, a name or an
/// existence check it does not have.
#[derive(Debug, Clone, Serialize)]
pub struct ActionTarget {
    pub kind: TargetKind,
    /// What the segmented chip renders.
    pub label: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_col: Option<u32>,
    /// The identifier (`symbol`) or the enclosing construct's name
    /// (`enclosing`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// The selected text, for a `text` target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// `path` targets only: does the repo actually have this file? Never a
    /// guess — an unknown answer omits the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// The blob this target was read from, when a read happened.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    /// Honest caption: why this target is what it is, or what could not be
    /// determined. Rendered under the chip.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// --- actions --------------------------------------------------------------

/// What the CLIENT does when a row is chosen. A CLOSED union: the SPA's
/// handler is exhaustive over it (TypeScript `never` check) and
/// `kb-code act` prints it verbatim.
///
/// Deliberately NOT an `href`. Root CLAUDE.md #35's rule is one URL builder;
/// the SPA's `lib/codeUrl.ts` is it, so this route hands over the ADDRESS
/// and never a second copy of the URL grammar.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ActionOp {
    /// Open a location in the reader.
    Open {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        line: Option<u32>,
        /// `2` = the other pane.
        pane: u8,
    },
    /// The zero-commitment rung — look without navigating.
    Peek { kind: &'static str },
    /// Open one of the reader's docks at this target.
    Dock { dock: &'static str },
    /// Run a kbcq/1 query (`search/grammar.rs`'s grammar, so the SPA's
    /// mirror parses it unchanged).
    Search { query: String },
    /// Put something on the clipboard. `value` is present only when the
    /// SERVER can know it; `permalink`/`snippet` are rendered client-side
    /// from the target (they need `location.origin` and the buffer's own
    /// text, neither of which this daemon has).
    Copy {
        what: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
    },
    /// Open a composer. `ask` is the durable question channel the agent
    /// drains; `annotate` is a note; `suggestion` is the existing
    /// suggestion editor (mutating).
    Compose { surface: &'static str },
    /// Add the target to a collection sink.
    Collect { sink: &'static str },
}

/// The daemon read that BACKS a row, when there is one. Lets `kb-code act`
/// actually perform the read half of the menu instead of only describing
/// it, and gives the SPA the exact query it would otherwise re-derive.
#[derive(Debug, Clone, Serialize)]
pub struct ActionRequest {
    pub method: &'static str,
    pub path: &'static str,
    pub query: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Action {
    /// Stable across releases. Never an ordinal — `kb-code act` refuses one
    /// by name.
    pub id: &'static str,
    /// Bumped when this row's MEANING changes. `(id, version)` is what a
    /// script may depend on.
    pub version: u32,
    pub group: &'static str,
    /// The letter inside the menu, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<&'static str>,
    pub title: &'static str,
    /// One line, rendered in the menu, in `--help` and in the CLI JSON —
    /// one string, three surfaces (Kakoune's `-docstring`).
    pub doc: &'static str,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    pub mutating: bool,
    /// Always `false` in v1 — see this module's rule 4.
    pub auto_navigate: bool,
    pub op: ActionOp,
    /// The `kb-code` command line that does the same thing, when one
    /// exists (F7). `None` is honest: some rows are browser-local.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<ActionRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionGroup {
    pub id: &'static str,
    pub title: &'static str,
    pub actions: Vec<Action>,
}

/// Why the mutating group is (or is not) present. A property of the
/// CALLER, stated plainly — a silent gap would read as "there is nothing
/// to do here".
#[derive(Debug, Clone, Serialize)]
pub struct MutationsNote {
    pub available: bool,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionsOut {
    pub schema: &'static str,
    pub repo: String,
    /// Ordered; index 0 is the default the gesture implies.
    pub targets: Vec<ActionTarget>,
    /// Which target the `groups` below were computed for.
    pub active: usize,
    pub groups: Vec<ActionGroup>,
    pub mutations: MutationsNote,
    /// Honest captions about anything that degraded.
    pub notes: Vec<String>,
}

// --- the ordered table ----------------------------------------------------

/// The group vocabulary and its render order — D5's own seven names.
pub const GROUPS: &[(&str, &str)] = &[
    ("navigate", "Go"),
    ("understand", "Understand"),
    ("find", "Find"),
    ("collect", "Collect"),
    ("provenance", "Cite"),
    ("agent", "Ask / Mark"),
    ("mutate", "Change"),
];

/// One row of the stable table: `(id, version, group, key, title, doc,
/// mutating)`. The ORDER of this array within a target kind is the render
/// order, and `golden_action_table_is_stable` pins it.
struct Spec {
    id: &'static str,
    version: u32,
    group: &'static str,
    key: Option<&'static str>,
    title: &'static str,
    doc: &'static str,
    mutating: bool,
}

const fn spec(
    id: &'static str,
    version: u32,
    group: &'static str,
    key: Option<&'static str>,
    title: &'static str,
    doc: &'static str,
) -> Spec {
    Spec {
        id,
        version,
        group,
        key,
        title,
        doc,
        mutating: false,
    }
}

const fn mut_spec(
    id: &'static str,
    version: u32,
    group: &'static str,
    key: Option<&'static str>,
    title: &'static str,
    doc: &'static str,
) -> Spec {
    Spec {
        id,
        version,
        group,
        key,
        title,
        doc,
        mutating: true,
    }
}

const SYMBOL_SPECS: &[Spec] = &[
    spec(
        "nav.peek-definition",
        1,
        "navigate",
        Some("p"),
        "Peek definition",
        "Look at the definition without leaving this file.",
    ),
    spec(
        "nav.definition",
        1,
        "navigate",
        Some("d"),
        "Go to definition",
        "Opens the disambiguation list; this route does not resolve, so nothing auto-navigates.",
    ),
    spec(
        "nav.open-pane2",
        1,
        "navigate",
        Some("O"),
        "Open in the other pane",
        "Put this file in pane 2 at this line, keeping the current one.",
    ),
    spec(
        "understand.hover",
        1,
        "understand",
        Some("k"),
        "Quick info",
        "Signature, doc and the originating change for this symbol.",
    ),
    spec(
        "find.usages",
        1,
        "find",
        Some("u"),
        "Usages",
        "The classified usages dock: census, chips, grouped tree, preview.",
    ),
    spec(
        "find.callers",
        1,
        "find",
        Some("c"),
        "Callers",
        "Call hierarchy inbound; four proof languages only.",
    ),
    spec(
        "find.text",
        1,
        "find",
        Some("/"),
        "Search this name as text",
        "A kbcq/1 text query over the repo.",
    ),
    spec(
        "collect.bookmark",
        1,
        "collect",
        Some("b"),
        "Bookmark this line",
        "Adds a bookmark at the caret, with an optional note.",
    ),
    spec(
        "provenance.copy-sym",
        1,
        "provenance",
        Some("y"),
        "Copy sym= address",
        "The symbol address, which survives code motion.",
    ),
    spec(
        "provenance.copy-permalink",
        1,
        "provenance",
        Some("l"),
        "Copy permalink",
        "Blob-sha pinned, so the link survives line drift.",
    ),
    spec(
        "provenance.copy-cli",
        1,
        "provenance",
        Some("x"),
        "Copy the CLI for this",
        "The kb-code command line that reproduces this menu.",
    ),
    spec(
        "agent.ask",
        1,
        "agent",
        Some("a"),
        "Ask here",
        "A durable question the agent drains, anchored to this target.",
    ),
    spec(
        "agent.annotate",
        1,
        "agent",
        Some("n"),
        "Annotate",
        "A note anchored to this target.",
    ),
    mut_spec(
        "mutate.suggest",
        1,
        "mutate",
        Some("s"),
        "Suggest an edit",
        "Opens the suggestion editor seeded with this range; apply is loopback-only.",
    ),
];

const ENCLOSING_SPECS: &[Spec] = &[
    spec(
        "nav.goto-enclosing",
        1,
        "navigate",
        Some("d"),
        "Go to this definition",
        "Jump to the enclosing construct's own first line.",
    ),
    spec(
        "find.usages",
        1,
        "find",
        Some("u"),
        "Usages of the enclosing def",
        "The classified usages dock, anchored at the definition.",
    ),
    spec(
        "find.callers",
        1,
        "find",
        Some("c"),
        "Callers",
        "Call hierarchy inbound; four proof languages only.",
    ),
    spec(
        "provenance.copy-permalink",
        1,
        "provenance",
        Some("l"),
        "Copy permalink",
        "Blob-sha pinned, so the link survives line drift.",
    ),
    spec(
        "provenance.copy-cli",
        1,
        "provenance",
        Some("x"),
        "Copy the CLI for this",
        "The kb-code command line that reproduces this menu.",
    ),
    spec(
        "agent.ask",
        1,
        "agent",
        Some("a"),
        "Ask here",
        "A durable question the agent drains, anchored to this target.",
    ),
    mut_spec(
        "mutate.suggest",
        1,
        "mutate",
        Some("s"),
        "Suggest an edit",
        "Opens the suggestion editor seeded with this range; apply is loopback-only.",
    ),
];

const RANGE_SPECS: &[Spec] = &[
    spec(
        "understand.blame-range",
        1,
        "understand",
        Some("b"),
        "Blame this range",
        "The range, not the file — who last touched each of these lines.",
    ),
    spec(
        "understand.why",
        1,
        "understand",
        Some("w"),
        "Why is this here",
        "The originating change and the session that wrote it.",
    ),
    spec(
        "find.text",
        1,
        "find",
        Some("/"),
        "Search this selection",
        "A kbcq/1 text query over the repo.",
    ),
    spec(
        "collect.bookmark",
        1,
        "collect",
        Some("b"),
        "Bookmark this range",
        "Adds a bookmark at the range start.",
    ),
    spec(
        "provenance.copy-permalink",
        1,
        "provenance",
        Some("l"),
        "Copy permalink",
        "Blob-sha pinned, so the link survives line drift.",
    ),
    spec(
        "provenance.copy-snippet",
        1,
        "provenance",
        Some("S"),
        "Copy with a provenance header",
        "The lines plus path, blob sha and range — a citable snippet.",
    ),
    spec(
        "provenance.copy-cli",
        1,
        "provenance",
        Some("x"),
        "Copy the CLI for this",
        "The kb-code command line that reproduces this menu.",
    ),
    spec(
        "agent.ask",
        1,
        "agent",
        Some("a"),
        "Ask here",
        "A durable question the agent drains, anchored to this range.",
    ),
    spec(
        "agent.annotate",
        1,
        "agent",
        Some("n"),
        "Annotate this range",
        "A note anchored to these lines.",
    ),
    mut_spec(
        "mutate.suggest",
        1,
        "mutate",
        Some("s"),
        "Suggest an edit",
        "Opens the suggestion editor seeded with this range; apply is loopback-only.",
    ),
];

const TEXT_SPECS: &[Spec] = &[
    spec(
        "find.text",
        1,
        "find",
        Some("/"),
        "Search as text",
        "A kbcq/1 text query over the repo.",
    ),
    spec(
        "find.regex",
        1,
        "find",
        Some("r"),
        "Search as a regex",
        "The same lane with the selection pre-escaped as a pattern.",
    ),
    spec(
        "find.symbol",
        1,
        "find",
        Some("s"),
        "Search as a symbol name",
        "The symbols lane, ranked by the one matcher.",
    ),
    spec(
        "provenance.copy-cli",
        1,
        "provenance",
        Some("x"),
        "Copy the CLI for this",
        "The kb-code command line that reproduces this menu.",
    ),
    spec(
        "agent.ask",
        1,
        "agent",
        Some("a"),
        "Ask here",
        "A durable question the agent drains, anchored to this selection.",
    ),
];

const PATH_SPECS: &[Spec] = &[
    spec(
        "nav.open",
        1,
        "navigate",
        Some("o"),
        "Open this file",
        "Opens in the focused pane.",
    ),
    spec(
        "nav.open-pane2",
        1,
        "navigate",
        Some("O"),
        "Open in the other pane",
        "Put this file in pane 2, keeping the current one.",
    ),
    spec(
        "understand.diagnostics",
        1,
        "understand",
        Some("g"),
        "Diagnostics here",
        "Whatever the configured language server reports for this file.",
    ),
    spec(
        "understand.framework",
        1,
        "understand",
        Some("f"),
        "Framework edges",
        "The Rails lens's convention edges; likely/candidate by construction.",
    ),
    spec(
        "collect.bookmark",
        1,
        "collect",
        Some("b"),
        "Bookmark this file",
        "Adds a bookmark at the file's first line.",
    ),
    spec(
        "provenance.copy-permalink",
        1,
        "provenance",
        Some("l"),
        "Copy permalink",
        "Blob-sha pinned, so the link survives line drift.",
    ),
    spec(
        "provenance.copy-cli",
        1,
        "provenance",
        Some("x"),
        "Copy the CLI for this",
        "The kb-code command line that reproduces this menu.",
    ),
    spec(
        "agent.ask",
        1,
        "agent",
        Some("a"),
        "Ask here",
        "A durable question the agent drains, anchored to this file.",
    ),
];

fn specs_for(kind: TargetKind) -> &'static [Spec] {
    match kind {
        TargetKind::Symbol => SYMBOL_SPECS,
        TargetKind::Range => RANGE_SPECS,
        TargetKind::Text => TEXT_SPECS,
        TargetKind::Path => PATH_SPECS,
        TargetKind::Enclosing => ENCLOSING_SPECS,
    }
}

// --- params ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ActionsParams {
    pub repo: String,
    pub path: String,
    pub line: u32,
    pub col: u32,
    #[serde(rename = "ref")]
    pub rev: Option<String>,
    /// Selection end, when the caller has a drag selection.
    pub end_line: Option<u32>,
    pub end_col: Option<u32>,
    /// The selected text, when there is one. Capped at
    /// [`MAX_TEXT_TARGET_BYTES`]; longer selections keep the `range`
    /// target and lose only the `text` interpretation, with a note.
    pub text: Option<String>,
    /// Which resolved target the groups are computed for (the segmented
    /// control's index). Out of range clamps to 0 and says so.
    pub target: Option<usize>,
}

// --- target resolution ----------------------------------------------------

/// A quoted `path:line` / `path` shape in selected text — the plumber's
/// first slice, restricted to the ONE routing this unit can honour without
/// a config table.
fn path_like(text: &str) -> Option<(String, Option<u32>)> {
    let t = text
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    if t.is_empty() || t.len() > 300 || t.contains(char::is_whitespace) {
        return None;
    }
    let (p, line) = match t.rsplit_once(':') {
        Some((head, tail)) if !head.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
            (head, tail.parse::<u32>().ok())
        }
        _ => (t, None),
    };
    if !p.contains('/') && !p.contains('.') {
        return None;
    }
    if p.starts_with('/') || p.contains("..") {
        return None;
    }
    Some((p.to_string(), line))
}

/// `true` when `text` is exactly one identifier token.
fn single_identifier(text: &str) -> bool {
    let t = text.trim();
    !t.is_empty()
        && t.len() <= 200
        && t.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '?' || c == '!')
        && !t.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn label_for_range(path: &str, a: u32, b: u32) -> String {
    if a == b {
        format!("{path}:{a}")
    } else {
        format!("{path}:{a}-{b}")
    }
}

/// The ordered target list. Deterministic and pure given its inputs, so the
/// ladder is unit-testable without a store.
#[allow(clippy::too_many_arguments)]
pub fn resolve_targets(
    path: &str,
    line: u32,
    col: u32,
    end_line: Option<u32>,
    end_col: Option<u32>,
    selected: Option<&str>,
    word: Option<&str>,
    enclosing: Option<(&str, Option<&str>, u32)>,
    blob_sha: Option<&str>,
    path_exists: impl Fn(&str) -> bool,
    notes: &mut Vec<String>,
) -> Vec<ActionTarget> {
    let mut out: Vec<ActionTarget> = Vec::new();
    let end_line = end_line.filter(|e| *e >= line);
    let multi = match (end_line, end_col) {
        (Some(e), _) if e > line => true,
        (Some(e), Some(ec)) if e == line && ec > col.saturating_add(1) => true,
        _ => false,
    };

    let sel_text = selected
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());
    let sel_too_big = sel_text
        .as_ref()
        .is_some_and(|s| s.len() > MAX_TEXT_TARGET_BYTES);
    if sel_too_big {
        notes.push(format!(
            "selection is {} bytes — over the {MAX_TEXT_TARGET_BYTES}-byte cap, so there is no text target",
            sel_text.as_ref().map(|s| s.len()).unwrap_or(0)
        ));
    }
    let sel_text = sel_text.filter(|_| !sel_too_big);

    let range_target = |note: Option<String>| ActionTarget {
        kind: TargetKind::Range,
        label: label_for_range(path, line, end_line.unwrap_or(line)),
        path: path.to_string(),
        line: Some(line),
        end_line: Some(end_line.unwrap_or(line)),
        col: Some(col),
        end_col,
        name: None,
        container: None,
        text: None,
        exists: None,
        blob_sha: blob_sha.map(str::to_string),
        note,
    };
    let text_target = |t: &str| ActionTarget {
        kind: TargetKind::Text,
        label: format!("{:?}", t.trim()),
        path: path.to_string(),
        line: Some(line),
        end_line,
        col: Some(col),
        end_col,
        name: None,
        container: None,
        text: Some(t.to_string()),
        exists: None,
        blob_sha: blob_sha.map(str::to_string),
        note: None,
    };
    let symbol_target = |name: &str| ActionTarget {
        kind: TargetKind::Symbol,
        label: name.to_string(),
        path: path.to_string(),
        line: Some(line),
        end_line: None,
        col: Some(col),
        end_col: None,
        name: Some(name.to_string()),
        container: None,
        text: None,
        exists: None,
        blob_sha: blob_sha.map(str::to_string),
        note: Some(
            "this route does not run the resolve ladder, so no trust class is claimed here"
                .to_string(),
        ),
    };

    // §2.3's ladder. A multi-token/multi-line selection means `range` first;
    // a caret or a single-identifier selection means `symbol` first.
    if multi {
        out.push(range_target(None));
        if let Some(t) = &sel_text {
            out.push(text_target(t));
            if single_identifier(t) {
                out.push(symbol_target(t.trim()));
            }
        }
    } else {
        if let Some(w) = word {
            out.push(symbol_target(w));
        }
        if let Some(t) = &sel_text {
            out.push(text_target(t));
        } else if word.is_none() {
            notes.push("no identifier at the caret — the file and range targets stand".to_string());
        }
        out.push(range_target(None));
    }

    // The plumber's one built-in shape: a path-looking selection becomes a
    // `path` target, verified against the repo rather than guessed.
    if let Some(t) = &sel_text {
        if let Some((p, l)) = path_like(t) {
            if p != path {
                let exists = path_exists(&p);
                out.push(ActionTarget {
                    kind: TargetKind::Path,
                    label: match l {
                        Some(n) => format!("{p}:{n}"),
                        None => p.clone(),
                    },
                    path: p.clone(),
                    line: l,
                    end_line: None,
                    col: None,
                    end_col: None,
                    name: None,
                    container: None,
                    text: None,
                    exists: Some(exists),
                    blob_sha: None,
                    note: (!exists).then(|| "this path is not in the repo index".to_string()),
                });
            }
        }
    }

    if let Some((name, container, def_line)) = enclosing {
        out.push(ActionTarget {
            kind: TargetKind::Enclosing,
            label: match container {
                Some(c) => format!("{c}#{name}"),
                None => name.to_string(),
            },
            path: path.to_string(),
            line: Some(def_line),
            end_line: None,
            col: None,
            end_col: None,
            name: Some(name.to_string()),
            container: container.map(str::to_string),
            text: None,
            exists: None,
            blob_sha: blob_sha.map(str::to_string),
            note: None,
        });
    } else {
        notes.push(
            "no enclosing definition — the file has no symbol covering this line".to_string(),
        );
    }

    // The open FILE is always a target, and always last: it is the one that
    // is never wrong, and it must never displace a narrower one.
    out.push(ActionTarget {
        kind: TargetKind::Path,
        label: path.to_string(),
        path: path.to_string(),
        line: Some(line),
        end_line: None,
        col: None,
        end_col: None,
        name: None,
        container: None,
        text: None,
        exists: Some(true),
        blob_sha: blob_sha.map(str::to_string),
        note: None,
    });

    out
}

// --- action building ------------------------------------------------------

fn q(pairs: &[(&str, String)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

/// The `kb-code act` line that re-opens this exact menu — the F7 row's own
/// payload, and what `provenance.copy-cli` copies.
pub fn act_cli_line(repo: &str, t: &ActionTarget) -> String {
    let at = match (t.line, t.end_line) {
        (Some(a), Some(b)) if b > a => format!("{}:{a}-{b}", t.path),
        (Some(a), _) => match t.col {
            Some(c) => format!("{}:{a}:{c}", t.path),
            None => format!("{}:{a}", t.path),
        },
        _ => t.path.clone(),
    };
    format!("kb-code act --list {at} --repo {repo} --json")
}

/// Build the ordered group list for one target.
fn build_groups(repo: &str, t: &ActionTarget, mutations_ok: bool) -> Vec<ActionGroup> {
    let mut groups: Vec<ActionGroup> = GROUPS
        .iter()
        .map(|(id, title)| ActionGroup {
            id,
            title,
            actions: Vec::new(),
        })
        .collect();

    for s in specs_for(t.kind) {
        if s.mutating && !mutations_ok {
            // ABSENT, not disabled (rule 2).
            continue;
        }
        let Some((op, cli, request, enabled, disabled_reason)) = op_for(repo, t, s.id) else {
            continue;
        };
        let action = Action {
            id: s.id,
            version: s.version,
            group: s.group,
            key: s.key,
            title: s.title,
            doc: s.doc,
            enabled,
            disabled_reason,
            mutating: s.mutating,
            auto_navigate: false,
            op,
            cli,
            request,
        };
        if let Some(g) = groups.iter_mut().find(|g| g.id == s.group) {
            g.actions.push(action);
        }
    }
    groups.retain(|g| !g.actions.is_empty());
    groups
}

type OpBuild = (
    ActionOp,
    Option<String>,
    Option<ActionRequest>,
    bool,
    Option<String>,
);

/// One row's operation + its CLI mirror + the daemon read that backs it.
///
/// Returns `None` only for an id that is not in this kind's table, which
/// `every_spec_builds_an_op` proves cannot happen — the option exists so
/// the two tables cannot silently disagree.
fn op_for(repo: &str, t: &ActionTarget, id: &str) -> Option<OpBuild> {
    let line = t.line.unwrap_or(1);
    let col = t.col.unwrap_or(0);
    let pos = format!("{}:{line}:{col}", t.path);
    let name = t.name.clone().unwrap_or_default();
    let text = t.text.clone().unwrap_or_else(|| name.clone());
    let enabled = true;
    let build = match id {
        "nav.peek-definition" => (
            ActionOp::Peek { kind: "definition" },
            Some(format!("kb-code resolve {pos} --repo {repo}")),
            Some(ActionRequest {
                method: "GET",
                path: "/api/resolve",
                query: q(&[
                    ("repo", repo.to_string()),
                    ("path", t.path.clone()),
                    ("line", line.to_string()),
                    ("col", col.to_string()),
                ]),
            }),
            enabled,
            None,
        ),
        "nav.definition" => (
            ActionOp::Dock {
                dock: "definitions",
            },
            Some(format!("kb-code resolve {pos} --repo {repo}")),
            Some(ActionRequest {
                method: "GET",
                path: "/api/resolve",
                query: q(&[
                    ("repo", repo.to_string()),
                    ("path", t.path.clone()),
                    ("line", line.to_string()),
                    ("col", col.to_string()),
                ]),
            }),
            enabled,
            None,
        ),
        "nav.goto-enclosing" | "nav.open" => (
            ActionOp::Open {
                path: t.path.clone(),
                line: t.line,
                pane: 1,
            },
            Some(format!("kb-code file {} --repo {repo}", t.path)),
            None,
            t.exists.unwrap_or(true),
            (t.exists == Some(false)).then(|| "not in the repo index".to_string()),
        ),
        "nav.open-pane2" => (
            ActionOp::Open {
                path: t.path.clone(),
                line: t.line,
                pane: 2,
            },
            None,
            None,
            t.exists.unwrap_or(true),
            (t.exists == Some(false)).then(|| "not in the repo index".to_string()),
        ),
        "understand.hover" => (
            ActionOp::Peek { kind: "hover" },
            Some(format!("kb-code hover {pos} --repo {repo}")),
            Some(ActionRequest {
                method: "GET",
                path: "/api/hover",
                query: q(&[
                    ("repo", repo.to_string()),
                    ("path", t.path.clone()),
                    ("line", line.to_string()),
                    ("col", col.to_string()),
                ]),
            }),
            enabled,
            None,
        ),
        "understand.blame-range" => (
            ActionOp::Dock { dock: "blame" },
            Some(format!("kb-code blame {} --repo {repo}", t.path)),
            Some(ActionRequest {
                method: "GET",
                path: "/api/blame",
                query: q(&[("repo", repo.to_string()), ("path", t.path.clone())]),
            }),
            enabled,
            None,
        ),
        "understand.why" => (
            ActionOp::Dock { dock: "why" },
            Some(format!("kb-code why {} --repo {repo}", t.path)),
            Some(ActionRequest {
                method: "GET",
                path: "/api/why",
                query: q(&[
                    ("repo", repo.to_string()),
                    ("path", t.path.clone()),
                    ("line", line.to_string()),
                ]),
            }),
            enabled,
            None,
        ),
        "understand.diagnostics" => (
            ActionOp::Dock {
                dock: "diagnostics",
            },
            Some(format!("kb-code diagnostics {} --repo {repo}", t.path)),
            Some(ActionRequest {
                method: "GET",
                path: "/api/diagnostics",
                query: q(&[("repo", repo.to_string()), ("path", t.path.clone())]),
            }),
            enabled,
            None,
        ),
        "understand.framework" => (
            ActionOp::Dock { dock: "framework" },
            Some(format!("kb-code framework {} --repo {repo}", t.path)),
            Some(ActionRequest {
                method: "GET",
                path: "/api/framework/edges",
                query: q(&[("repo", repo.to_string()), ("path", t.path.clone())]),
            }),
            enabled,
            None,
        ),
        "find.usages" => (
            ActionOp::Dock { dock: "usages" },
            Some(format!("kb-code usages {pos} --repo {repo} --v2")),
            Some(ActionRequest {
                method: "GET",
                path: "/api/usages/2",
                query: q(&[
                    ("repo", repo.to_string()),
                    ("path", t.path.clone()),
                    ("line", line.to_string()),
                    ("col", col.to_string()),
                ]),
            }),
            enabled,
            None,
        ),
        "find.callers" => (
            ActionOp::Dock { dock: "callers" },
            Some(format!("kb-code callers {pos} --repo {repo}")),
            Some(ActionRequest {
                method: "GET",
                path: "/api/hierarchy/callers",
                query: q(&[
                    ("repo", repo.to_string()),
                    ("path", t.path.clone()),
                    ("line", line.to_string()),
                    ("col", col.to_string()),
                ]),
            }),
            enabled,
            None,
        ),
        "find.text" => {
            let query = format!("repo:{repo} /{}", text.trim());
            (
                ActionOp::Search {
                    query: query.clone(),
                },
                Some(format!("kb-code search {query:?}")),
                None,
                !text.trim().is_empty(),
                text.trim()
                    .is_empty()
                    .then(|| "nothing selected and no identifier at the caret".to_string()),
            )
        }
        "find.regex" => {
            let query = format!("repo:{repo} /{}/", regex_escape(text.trim()));
            (
                ActionOp::Search {
                    query: query.clone(),
                },
                Some(format!("kb-code search {query:?}")),
                None,
                !text.trim().is_empty(),
                text.trim()
                    .is_empty()
                    .then(|| "nothing selected".to_string()),
            )
        }
        "find.symbol" => {
            let query = format!("repo:{repo} @{}", text.trim());
            (
                ActionOp::Search {
                    query: query.clone(),
                },
                Some(format!("kb-code search {query:?}")),
                None,
                !text.trim().is_empty(),
                text.trim()
                    .is_empty()
                    .then(|| "nothing selected".to_string()),
            )
        }
        "collect.bookmark" => (
            ActionOp::Collect { sink: "bookmark" },
            Some(format!(
                "kb-code bookmark add {}:{line} --repo {repo}",
                t.path
            )),
            None,
            enabled,
            None,
        ),
        "provenance.copy-sym" => {
            let value = t.name.clone();
            (
                ActionOp::Copy {
                    what: "sym",
                    value: value.clone(),
                },
                value
                    .as_ref()
                    .map(|v| format!("kb-code resolve-symbol {v:?}")),
                None,
                value.is_some(),
                value
                    .is_none()
                    .then(|| "no symbol name at this target".to_string()),
            )
        }
        "provenance.copy-permalink" => (
            ActionOp::Copy {
                what: "permalink",
                value: None,
            },
            None,
            None,
            enabled,
            None,
        ),
        "provenance.copy-snippet" => (
            ActionOp::Copy {
                what: "snippet",
                value: None,
            },
            None,
            None,
            enabled,
            None,
        ),
        "provenance.copy-cli" => {
            let v = act_cli_line(repo, t);
            (
                ActionOp::Copy {
                    what: "cli",
                    value: Some(v.clone()),
                },
                Some(v),
                None,
                enabled,
                None,
            )
        }
        "agent.ask" => (
            ActionOp::Compose { surface: "ask" },
            Some(format!(
                "kb-code annotate add {}:{line} --repo {repo} --intent question --body '…'",
                t.path
            )),
            None,
            enabled,
            None,
        ),
        "agent.annotate" => (
            ActionOp::Compose {
                surface: "annotate",
            },
            Some(format!(
                "kb-code annotate add {}:{line} --repo {repo} --body '…'",
                t.path
            )),
            None,
            enabled,
            None,
        ),
        "mutate.suggest" => (
            ActionOp::Compose {
                surface: "suggestion",
            },
            Some(format!(
                "kb-code suggest create --repo {repo} --path {} --line {line}",
                t.path
            )),
            None,
            enabled,
            None,
        ),
        _ => return None,
    };
    Some(build)
}

/// Minimal regex metacharacter escaping for `find.regex` — the selection is
/// searched LITERALLY, so every metacharacter is quoted. Deliberately its
/// own three lines rather than a dependency: this escapes for the `regex`
/// crate AND for ECMAScript `RegExp`, and both accept a backslash before any
/// ASCII punctuation.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if c.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// --- the route ------------------------------------------------------------

/// `GET /api/actions?repo=&path=&line=&col=[&ref=][&end_line=][&end_col=][&text=][&target=]`.
///
/// Reads `ConnectInfo` directly, the same carve-out `routes::repos` and
/// `search::unified` already take and for the same reason: the answer
/// depends on whether THIS caller is a loopback peer (rule 2). Everything
/// else is a cheap read — one file read, one symbol-table lookup — so the
/// menu can open inside the 50 ms budget risk 7 sets.
pub async fn actions_route(
    State(state): State<SharedState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(params): Query<ActionsParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo = repo.clone();

    let loopback = kb_server::middleware::is_loopback_origin(
        Some(peer.ip()),
        &headers,
        &state.auth.trusted_proxies,
    );
    let mutations = if loopback {
        MutationsNote {
            available: true,
            reason: "loopback caller",
        }
    } else if state.review.remote_mutations {
        MutationsNote {
            available: true,
            reason: "[review] remote_mutations is on",
        }
    } else {
        MutationsNote {
            available: false,
            reason: "mutating actions are omitted: this request did not arrive over loopback",
        }
    };

    let repo_bg = repo.clone();
    let path = params.path.clone();
    let line = params.line;
    let col = params.col;
    let rev = params.rev.clone();
    let selected = params.text.clone();
    let end_line = params.end_line;
    let end_col = params.end_col;

    // 2026-08-31 incident (store.rs module doc): the whole synchronous half
    // — one file read plus one symbol-table lookup — is ONE blocking hop.
    let (targets, mut notes) = state
        .store
        .run_blocking(move |store| {
            let mut notes: Vec<String> = Vec::new();
            let read = read_repo_file(&repo_bg, &path, rev.as_deref()).ok();
            let blob_sha = read.as_ref().map(|r| r.blob_hash.clone());
            let lang = crate::lang::detect(&path, read.as_ref().map(|r| r.bytes.as_slice()));
            let word = read.as_ref().and_then(|r| {
                let text = std::str::from_utf8(&r.bytes).ok()?;
                let l = text.lines().nth(line.saturating_sub(1) as usize)?;
                crate::resolve::word_at(l.as_bytes(), col as usize)
            });
            let symbols = match (&blob_sha, lang) {
                (Some(h), Some(l)) => store.symbols_for_blob(h, l.salt).unwrap_or_default(),
                _ => Vec::new(),
            };
            if symbols.is_empty() && read.is_some() {
                notes.push(
                    "no symbol table for this blob — the enclosing target is unavailable"
                        .to_string(),
                );
            }
            let enc = crate::annotations::enclosing_symbol(&symbols, line);
            let enclosing = enc.map(|s| (s.name.as_str(), s.container.as_deref(), s.line_start));
            let targets = resolve_targets(
                &path,
                line,
                col,
                end_line,
                end_col,
                selected.as_deref(),
                word.as_deref(),
                enclosing,
                blob_sha.as_deref(),
                |p| store.get_file(repo_id, p).ok().flatten().is_some(),
                &mut notes,
            );
            (targets, notes)
        })
        .await;

    if targets.is_empty() {
        return Err(ApiError::bad_request(
            "no target could be resolved at that position",
        ));
    }
    let requested = params.target.unwrap_or(0);
    let active = if requested < targets.len() {
        requested
    } else {
        notes.push(format!(
            "target index {requested} is out of range ({} resolved) — showing the default",
            targets.len()
        ));
        0
    };
    let groups = build_groups(&params.repo, &targets[active], mutations.available);

    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(ActionsOut {
            schema: ACTIONS_SCHEMA,
            repo: params.repo.clone(),
            targets,
            active,
            groups,
            mutations,
            notes,
        }),
    ))
}

// --- the surface declaration (invariant 15) --------------------------------

pub const ACTIONS_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/actions",
    handler: "actions::actions_route",
    required_params: &["repo", "path", "line", "col"],
    params_accept_without: actions_params_accept_without,
};

fn actions_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [
        ("repo", serde_json::json!("r")),
        ("path", serde_json::json!("a.rb")),
        ("line", serde_json::json!(1u32)),
        ("col", serde_json::json!(0u32)),
    ] {
        if k != omit {
            map.insert(k.to_string(), v);
        }
    }
    serde_json::from_value::<ActionsParams>(serde_json::Value::Object(map)).is_ok()
}

/// Every route unit V71-E2 adds. Walked from BOTH sides — this crate's
/// `every_declared_v71_e2_route_is_registered_and_requires_its_params` and
/// kb-code-cli's `cli_requests_send_every_param_their_route_requires`.
/// Adding a route without adding it here is the one gap neither test can
/// see, which is why the list lives beside the route.
pub const V71_E2_ROUTES: &[crate::entities::RouteContract] = &[ACTIONS_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("router.rs");

    fn target(kind: TargetKind) -> ActionTarget {
        ActionTarget {
            kind,
            label: "x".into(),
            path: "app/models/order.rb".into(),
            line: Some(88),
            end_line: Some(92),
            col: Some(4),
            end_col: Some(9),
            name: Some("total".into()),
            container: Some("Order".into()),
            text: Some("total".into()),
            exists: Some(true),
            blob_sha: Some("9f2c".into()),
            note: None,
        }
    }

    #[test]
    fn every_declared_v71_e2_route_is_registered_and_requires_its_params() {
        assert!(!V71_E2_ROUTES.is_empty());
        for c in V71_E2_ROUTES {
            let nested = c
                .path
                .strip_prefix("/api")
                .expect("every route path is /api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: declared but never registered in router.rs — the v7.0 dead-surface defect",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{}: router.rs never names the handler {}",
                c.path,
                c.handler
            );
            assert!(
                (c.params_accept_without)(""),
                "{}: the complete param map must deserialize",
                c.path
            );
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: params struct still deserializes without the required `{p}`",
                    c.path
                );
            }
        }
    }

    /// D5: "Ask here" is a REQUIRED row for every target kind.
    #[test]
    fn every_target_kind_offers_ask_here() {
        for kind in TargetKind::ALL {
            let groups = build_groups("r", &target(*kind), true);
            let has = groups
                .iter()
                .flat_map(|g| &g.actions)
                .any(|a| a.id == "agent.ask");
            assert!(has, "{} has no `agent.ask` row", kind.as_str());
        }
    }

    /// Every spec row must build an op, and every built op must belong to a
    /// declared group — the two tables cannot silently disagree.
    #[test]
    fn every_spec_builds_an_op_in_a_declared_group() {
        for kind in TargetKind::ALL {
            let t = target(*kind);
            for s in specs_for(*kind) {
                assert!(
                    op_for("r", &t, s.id).is_some(),
                    "{}/{}: declared in the spec table with no op — dead surface",
                    kind.as_str(),
                    s.id
                );
                assert!(
                    GROUPS.iter().any(|(id, _)| *id == s.group),
                    "{}/{}: group {:?} is not in GROUPS",
                    kind.as_str(),
                    s.id,
                    s.group
                );
            }
        }
    }

    /// Ordering is stable per target kind (risk 6 — reordering destroys
    /// muscle memory). This golden is the version pin: changing a row's
    /// position, id or version fails here by name.
    #[test]
    fn golden_action_table_is_stable() {
        let rendered: Vec<String> = TargetKind::ALL
            .iter()
            .map(|k| {
                let rows: Vec<String> = specs_for(*k)
                    .iter()
                    .map(|s| format!("{}:{}@{}", s.group, s.id, s.version))
                    .collect();
                format!("{} = {}", k.as_str(), rows.join(" "))
            })
            .collect();
        assert_eq!(
            rendered,
            vec![
                "symbol = navigate:nav.peek-definition@1 navigate:nav.definition@1 navigate:nav.open-pane2@1 understand:understand.hover@1 find:find.usages@1 find:find.callers@1 find:find.text@1 collect:collect.bookmark@1 provenance:provenance.copy-sym@1 provenance:provenance.copy-permalink@1 provenance:provenance.copy-cli@1 agent:agent.ask@1 agent:agent.annotate@1 mutate:mutate.suggest@1",
                "range = understand:understand.blame-range@1 understand:understand.why@1 find:find.text@1 collect:collect.bookmark@1 provenance:provenance.copy-permalink@1 provenance:provenance.copy-snippet@1 provenance:provenance.copy-cli@1 agent:agent.ask@1 agent:agent.annotate@1 mutate:mutate.suggest@1",
                "text = find:find.text@1 find:find.regex@1 find:find.symbol@1 provenance:provenance.copy-cli@1 agent:agent.ask@1",
                "path = navigate:nav.open@1 navigate:nav.open-pane2@1 understand:understand.diagnostics@1 understand:understand.framework@1 collect:collect.bookmark@1 provenance:provenance.copy-permalink@1 provenance:provenance.copy-cli@1 agent:agent.ask@1",
                "enclosing = navigate:nav.goto-enclosing@1 find:find.usages@1 find:find.callers@1 provenance:provenance.copy-permalink@1 provenance:provenance.copy-cli@1 agent:agent.ask@1 mutate:mutate.suggest@1",
            ]
        );
    }

    /// Rule 2 — the mutating rows are ABSENT, not disabled, for a caller
    /// the server did not clear.
    #[test]
    fn mutating_rows_are_absent_not_disabled_for_an_ungated_caller() {
        for kind in TargetKind::ALL {
            let t = target(*kind);
            let with = build_groups("r", &t, true);
            let without = build_groups("r", &t, false);
            let mutating_ids: Vec<&str> = specs_for(*kind)
                .iter()
                .filter(|s| s.mutating)
                .map(|s| s.id)
                .collect();
            for id in &mutating_ids {
                assert!(
                    with.iter().flat_map(|g| &g.actions).any(|a| a.id == *id),
                    "{}: {id} missing for a cleared caller",
                    kind.as_str()
                );
                assert!(
                    !without.iter().flat_map(|g| &g.actions).any(|a| a.id == *id),
                    "{}: {id} still rendered (even disabled) for an ungated caller",
                    kind.as_str()
                );
            }
            if !mutating_ids.is_empty() {
                assert!(
                    !without.iter().any(|g| g.id == "mutate"),
                    "{}: the Change group must vanish entirely, not render empty",
                    kind.as_str()
                );
            }
        }
    }

    /// Rule 4 — nothing auto-navigates, because this route never resolves.
    #[test]
    fn no_row_ever_auto_navigates() {
        for kind in TargetKind::ALL {
            for a in build_groups("r", &target(*kind), true)
                .iter()
                .flat_map(|g| &g.actions)
            {
                assert!(
                    !a.auto_navigate,
                    "{}/{} auto-navigates",
                    kind.as_str(),
                    a.id
                );
            }
        }
    }

    #[test]
    fn a_multi_line_selection_puts_range_first_and_a_caret_puts_symbol_first() {
        let mut notes = Vec::new();
        let multi = resolve_targets(
            "a.rb",
            10,
            0,
            Some(14),
            Some(3),
            Some("a = 1\nb = 2"),
            Some("a"),
            Some(("run", Some("Job"), 8)),
            None,
            |_| false,
            &mut notes,
        );
        assert_eq!(multi[0].kind, TargetKind::Range);
        assert_eq!(multi[1].kind, TargetKind::Text);

        let mut notes = Vec::new();
        let caret = resolve_targets(
            "a.rb",
            10,
            4,
            None,
            None,
            None,
            Some("total"),
            Some(("run", Some("Job"), 8)),
            None,
            |_| false,
            &mut notes,
        );
        assert_eq!(caret[0].kind, TargetKind::Symbol);
        assert_eq!(caret[0].name.as_deref(), Some("total"));
        // The open file is always last and always a `path`.
        assert_eq!(caret.last().unwrap().kind, TargetKind::Path);
    }

    #[test]
    fn a_single_identifier_selection_still_offers_the_symbol_reading() {
        let mut notes = Vec::new();
        let ts = resolve_targets(
            "a.rb",
            10,
            0,
            Some(10),
            Some(9),
            Some("total"),
            Some("total"),
            None,
            None,
            |_| false,
            &mut notes,
        );
        assert_eq!(ts[0].kind, TargetKind::Range);
        assert!(ts.iter().any(|t| t.kind == TargetKind::Symbol));
    }

    #[test]
    fn a_path_shaped_selection_is_verified_never_guessed() {
        let mut notes = Vec::new();
        let ts = resolve_targets(
            "a.rb",
            1,
            0,
            None,
            None,
            Some("app/models/user.rb:42"),
            None,
            None,
            None,
            |p| p == "app/models/user.rb",
            &mut notes,
        );
        let p = ts
            .iter()
            .find(|t| t.kind == TargetKind::Path && t.path == "app/models/user.rb")
            .expect("the plumber's path target");
        assert_eq!(p.exists, Some(true));
        assert_eq!(p.line, Some(42));

        let mut notes = Vec::new();
        let ts = resolve_targets(
            "a.rb",
            1,
            0,
            None,
            None,
            Some("app/models/ghost.rb"),
            None,
            None,
            None,
            |_| false,
            &mut notes,
        );
        let p = ts
            .iter()
            .find(|t| t.kind == TargetKind::Path && t.path == "app/models/ghost.rb")
            .expect("the target still appears");
        assert_eq!(p.exists, Some(false));
        assert!(p.note.is_some(), "a missing path must say so");
    }

    #[test]
    fn path_like_refuses_an_escape_or_a_bare_word() {
        assert!(path_like("hello").is_none());
        assert!(path_like("/etc/passwd").is_none());
        assert!(path_like("../../secret").is_none());
        assert!(path_like("a b/c.rb").is_none());
        assert_eq!(path_like("lib/x.rb"), Some(("lib/x.rb".to_string(), None)));
    }

    #[test]
    fn an_oversized_selection_keeps_the_range_and_says_why() {
        let big = "x".repeat(MAX_TEXT_TARGET_BYTES + 1);
        let mut notes = Vec::new();
        let ts = resolve_targets(
            "a.rb",
            1,
            0,
            Some(9),
            None,
            Some(&big),
            None,
            None,
            None,
            |_| false,
            &mut notes,
        );
        assert!(ts.iter().all(|t| t.kind != TargetKind::Text));
        assert!(notes.iter().any(|n| n.contains("byte cap")));
    }

    #[test]
    fn regex_escape_quotes_every_metacharacter() {
        assert_eq!(regex_escape("a.b(c)"), r"a\.b\(c\)");
        assert_eq!(regex_escape("plain"), "plain");
    }

    #[test]
    fn the_copy_cli_row_carries_the_line_that_reopens_this_menu() {
        let t = target(TargetKind::Range);
        let groups = build_groups("myrepo", &t, true);
        let row = groups
            .iter()
            .flat_map(|g| &g.actions)
            .find(|a| a.id == "provenance.copy-cli")
            .unwrap();
        match &row.op {
            ActionOp::Copy { what, value } => {
                assert_eq!(*what, "cli");
                let v = value.as_deref().unwrap();
                assert!(
                    v.starts_with("kb-code act --list app/models/order.rb:88-92"),
                    "{v}"
                );
                assert!(v.contains("--repo myrepo"));
            }
            other => panic!("unexpected op {other:?}"),
        }
    }
}
