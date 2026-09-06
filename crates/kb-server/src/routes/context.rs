//! CT-D1 — `GET /api/context` : the ONE deterministic, budgeted context pack.
//!
//! The recorded deferral of `kb context` (fresh-eyes #10) named exactly one
//! blocker: **no caller**. The caller exists — `plugins/kb-memory/hooks/
//! kb-recall.sh` receives the real task text and cwd on the first
//! `UserPromptSubmit`. This route is that pack.
//!
//! ## What it is
//!
//! A **composer, not a fifth assembler**. Every lane below is an EXISTING
//! read, called in-process:
//!
//! | lane | source | invariant it must not bend |
//! |------|--------|----------------------------|
//! | `memories` | [`crate::routes::memory::recall_compose`] | #10 — surfaced, never scored |
//! | `sessions` | [`crate::routes::sessions::recollect_compose`] | #11 R0/R1/R3 — POINTERS only |
//! | `comments` | [`crate::routes::inbox::collect_open`] | #6 — `review::load`, never hand-parsed |
//! | `code_hints` | `Storage::code_refs_of` (kb-LOCAL `code_refs`) | #2 — HINTS, never a trust class |
//!
//! Nothing here is persisted, nothing here calls an LLM (the no-in-daemon-LLM
//! non-goal), and nothing here calls kb-code (invariant #2's ONE live call
//! direction is kb-code→kb; a blocking cross-daemon call inside the
//! `UserPromptSubmit` hot path was already killed once as a reliability
//! hazard — CT-C4).
//!
//! ## R0 / R3 stay intact because the pack POINTS, the agent PULLS
//!
//! The sessions lane returns session ids, display names, the surfaced
//! recency/error/commit signals, and a ONE-LINE excerpt of the R1 **digest**
//! — never a transcript body, never the raw JSONL. That is exactly what
//! `GET /api/sessions/recollect` already returns; this route narrows it
//! (fewer fields, a hard char cap) rather than widening it. The hook injects
//! only the `scent` COUNTS line ("3 prior sessions · 2 open comments · 5
//! memories — run `kb context`"), so nothing episodic is auto-injected: the
//! agent decides to pull. R3's "success/staleness are SURFACED signals, never
//! score terms" survives verbatim — `stale`/`error_count`/`commit_count` ride
//! along untouched and this route adds no ranking of its own beyond the
//! documented cwd partition below.
//!
//! ## The budget is HARD and its truncation is always EXPLICIT
//!
//! Mirrors the CT-E1 daycard lane-cap precedent (`*_truncated` flags). Each
//! lane gets a fixed PERCENTAGE share of the char budget
//! ([`LANE_SHARE_MEMORIES`] &c.) plus its own item cap; leftovers are
//! deliberately NOT redistributed, so a lane's contents can never be
//! perturbed by a sibling lane's size (each lane is independently
//! reproducible from its own inputs — the property
//! `lanes_are_independent_of_each_other` pins). Every drop is reported:
//! `<lane>_total` carries the pre-cap count, `<lane>_truncated` says the list
//! is short, and `budget_exceeded` says the CHAR budget (not an item cap)
//! caused at least one of those drops. A budget small enough to fit nothing
//! yields honest empty lanes with the flags set — never a silent half-pack.
//!
//! ## Determinism
//!
//! Same corpora + same query ⇒ same pack. Fan-out is `buffered_join`
//! (invariant #28, submission order); every sort carries a total-order
//! tiebreak; the cwd preference is a STABLE partition (see
//! [`partition_by_cwd`]).

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    extract::{Extension, Query, State},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

// ---- budget + caps ---------------------------------------------------------

/// Default char budget for the whole pack. Sized so the rendered human pack
/// stays a glanceable page rather than a context-window tax.
pub const CONTEXT_DEFAULT_BUDGET: u32 = 4_000;
/// Floor + ceiling for `?budget=`. The floor is deliberately LOW enough to be
/// unusable (a caller asking for 200 chars gets honest near-empty lanes with
/// `budget_exceeded: true`) — clamping to something "sensible" would hide the
/// caller's own mistake.
const CONTEXT_MIN_BUDGET: u32 = 200;
const CONTEXT_MAX_BUDGET: u32 = 32_000;

/// Per-lane share of the char budget, in percent. Must sum to 100.
const LANE_SHARE_MEMORIES: u32 = 40;
const LANE_SHARE_SESSIONS: u32 = 30;
const LANE_SHARE_COMMENTS: u32 = 20;
const LANE_SHARE_CODE_HINTS: u32 = 10;

/// Per-lane item caps, applied BEFORE the char budget.
const MEMORY_ITEM_CAP: usize = 5;
const SESSION_ITEM_CAP: usize = 5;
const COMMENT_ITEM_CAP: usize = 5;
const CODE_HINT_ITEM_CAP: usize = 10;

/// Candidate-pool sizes, deliberately LARGER than the item caps above.
///
/// Without the over-fetch a lane's `<lane>_total` would equal its item cap by
/// construction, so `<lane>_truncated` could only ever fire on the char
/// budget and the scent would report the cap back to itself. With it, both
/// numbers say something: `_total` is how many candidates the lane actually
/// had, `_truncated` says the item cap (or the budget) cut them down.
/// SESSION_POOL additionally gives [`partition_by_cwd`] material to reorder
/// before [`SESSION_ITEM_CAP`] bites.
const MEMORY_POOL: usize = 10;
const SESSION_POOL: u32 = 12;

/// Per-corpus candidate pool for the internal artifact-match step, and the
/// per-corpus cap on what survives it. The cap bounds the `code_refs_of`
/// read count (one per matched artifact) to `CAP × corpora` — this route is
/// a pull verb, but it is called from a `UserPromptSubmit` hook on turn 1,
/// so its IO must stay small and knowable.
const ARTIFACT_MATCH_POOL: u32 = 40;
const ARTIFACT_MATCH_CAP: usize = 8;

/// One-line excerpt caps. These are PER-ITEM shape caps (the pack points, it
/// does not carry bodies); the LANE truncation flags are a separate,
/// explicitly-reported thing.
const SESSION_EXCERPT_CHARS: usize = 160;
const COMMENT_EXCERPT_CHARS: usize = 160;
const MEMORY_SUMMARY_CHARS: usize = 220;

// ---- wire shape ------------------------------------------------------------

/// `GET /api/context` query params.
///
/// Deliberately NOT ts-exported: the pack's consumers are the CLI
/// (`kb context`) and the `kb-recall.sh` hook, not the SPA. Adding
/// `ts(export)` here would put four new files under
/// `web/src/api/generated/` with nothing importing them.
#[derive(Debug, Deserialize, Default)]
pub struct ContextParams {
    /// The task text. Required — an empty pack for an empty query would be a
    /// meaningless read.
    #[serde(default)]
    pub q: String,
    /// The caller's working directory. NOT a hard filter: it STABLY partitions
    /// the sessions lane (same-cwd first) and sets `same_cwd` per session. A
    /// hard filter would answer "nothing happened here" for every first task
    /// in a fresh checkout, which is worse than an honest fleet-wide ranking.
    pub cwd: Option<String>,
    /// Char budget for the whole pack; clamped to
    /// `[CONTEXT_MIN_BUDGET, CONTEXT_MAX_BUDGET]`.
    pub budget: Option<u32>,
    /// The CALLER's own session id. Excluded from the sessions lane — a long
    /// session is captured at every Stop (invariant #11 multi-capture), so
    /// without this the pack cheerfully tells you about yourself.
    pub session: Option<String>,
    /// Bypass the per-corpus salience/decay FLOOR on the memories lane, for
    /// this call only — a straight passthrough to
    /// `GET /api/memory/recall?no_floor=`, whose ranking math is unchanged
    /// (invariant #10 holds either way; the floor is a post-rank drop, not a
    /// score term).
    ///
    /// This exists for exactly one caller shape: `/kb-distill`'s dedup
    /// oracle, whose own text explains why — "the floor otherwise hides
    /// exactly the low-salience / decayed memories a distiller must see to
    /// avoid re-creating a near-duplicate". Default OFF, so the turn-1 hook
    /// path and every ordinary `kb context` read see the SAME floored
    /// population recall shows.
    #[serde(default)]
    pub no_floor: bool,
    /// CT-B2 (memory-scoping) — passthrough to `GET /api/memory/recall`'s
    /// `project`, narrowing the memories lane's `scope=all` fan-out to a
    /// named project corpus's own memories plus every global corpus
    /// (mirrors `kb recall`'s `--project`). Absent ⇒ byte-identical to
    /// pre-CT-B2 (every in-scope corpus, as before).
    pub memory_project: Option<String>,
    /// CT-B2 (memory-scoping) — passthrough to `GET /api/memory/recall`'s
    /// `visible_to` (see `crate::routes::memory::Params::visible_to` for
    /// the full filter semantics). Absent ⇒ byte-identical to pre-CT-B2
    /// (no visibility filtering on the memories lane).
    pub memory_visible_to: Option<String>,
}

/// One recalled memory, trimmed to the pack shape with its FULL invariant-#10
/// score decomposition intact (that decomposition is the whole reason a pack
/// hit is trustworthy — a bare title is an assertion, `score = rel × salience
/// × decay` is an argument).
#[derive(Debug, Serialize)]
pub struct ContextMemory {
    pub id: String,
    pub kb: String,
    pub title: String,
    pub source_relative: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub score: f32,
    pub salience: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decay: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_days: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance_factor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stability: Option<f32>,
    /// CT-C1 — an open `[kb-flag]` marker says a past agent found this WRONG.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub flagged: bool,
    /// CT-C3 — `kb remember --failed`: a tried-and-did-NOT-work memory.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub warns: bool,
    /// CT-C4 — open `[kb-drift]` markers (a citation rotted).
    #[serde(skip_serializing_if = "is_zero_u32", default)]
    pub drift_open: u32,
    /// L7 visibility — the memory is global (`*`).
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub global: bool,
    /// L7 visibility — the kbs this memory is explicitly linked to. Carried
    /// because `/kb-distill`'s Step 6 "visibility ladder" inherits a dedup
    /// neighbour's `linked_kbs`; without it that skill would have to call
    /// recall a second time just for this field, which is precisely the
    /// hand-chaining this route exists to delete.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub linked_kbs: Vec<String>,
}

impl ContextMemory {
    /// Chars this item charges against the memories lane budget.
    fn cost(&self) -> usize {
        chars(&self.title)
            + self.summary.as_deref().map(chars).unwrap_or(0)
            + chars(&self.kb)
            + chars(&self.id)
    }
}

/// One recollected session — a POINTER (id + name + one-line digest excerpt +
/// the surfaced signals), never a transcript. Invariant #11 R0/R1/R3.
#[derive(Debug, Serialize)]
pub struct ContextSession {
    pub session_id: String,
    pub kb: String,
    pub display_name: String,
    pub started_at: i64,
    /// Whole-day age — surfaced, never used to drop a hit (R3).
    pub age_days: i64,
    pub stale: bool,
    pub commit_count: u32,
    pub error_count: u32,
    /// One line of the R1 DIGEST (else the session's closure). Capped at
    /// [`SESSION_EXCERPT_CHARS`]; pull the rest with `kb sessions read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    /// This session ran in the `?cwd=` the caller passed.
    pub same_cwd: bool,
}

impl ContextSession {
    fn cost(&self) -> usize {
        chars(&self.display_name)
            + self.excerpt.as_deref().map(chars).unwrap_or(0)
            + chars(&self.session_id)
            + chars(&self.kb)
    }
}

/// One OPEN comment on an artifact this query matched.
#[derive(Debug, Serialize)]
pub struct ContextComment {
    pub kb: String,
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_relative: Option<String>,
    pub title: String,
    pub comment_id: String,
    pub excerpt: String,
    pub author: String,
    pub reply_count: u32,
    pub stale: bool,
    pub updated_at: i64,
}

impl ContextComment {
    fn cost(&self) -> usize {
        chars(&self.title) + chars(&self.excerpt) + chars(&self.comment_id) + chars(&self.kb)
    }
}

/// One code path the pack's own artifacts CITE — a kb-local `code_refs`
/// hint, invariant #2. This is emphatically NOT a claim that the path
/// exists, compiles, or still means what the citing doc thought: kb has no
/// tree and no symbols, so it is structurally incapable of minting a trust
/// class. `kb-code` does that.
#[derive(Debug, Serialize)]
pub struct ContextCodeHint {
    pub path_hint: String,
    /// How many of the pack's source artifacts cite this path.
    pub cited_by: u32,
}

impl ContextCodeHint {
    fn cost(&self) -> usize {
        chars(&self.path_hint)
    }
}

/// The pack.
#[derive(Debug, Serialize)]
pub struct ContextResponse {
    pub q: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The clamped budget actually applied (echoing it makes a surprising
    /// truncation self-explaining).
    pub budget: u32,
    /// The memories lane skipped the salience/decay floor. Echoed because a
    /// no-floor pack legitimately contains memories the ordinary floor
    /// hides — a reader must be able to tell which population they got.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub no_floor: bool,
    /// COUNTS ONLY — the line the `kb-recall.sh` scent branch injects on the
    /// first turn. Never carries substance; see the module doc.
    ///
    /// Its numbers are the `<lane>_total`s below, i.e. PRE-truncation. That
    /// is the honest figure to act on: acting means re-running with a bigger
    /// `budget`, and being told "2 memories" because 2 is all that fit would
    /// hide the reason to.
    pub scent: String,

    // Every `<lane>_total` is this lane's PRE-BUDGET, PRE-item-cap candidate
    // count — bounded by the lane's own pool (`MEMORY_POOL`, `SESSION_POOL`,
    // `ARTIFACT_MATCH_CAP × corpora`). It is NOT a corpus-wide census: a pack
    // is a pack. `kb memory census` / `GET /api/inbox` are the censuses.
    pub memories: Vec<ContextMemory>,
    pub memories_total: u32,
    pub memories_truncated: bool,

    pub sessions: Vec<ContextSession>,
    pub sessions_total: u32,
    pub sessions_truncated: bool,

    pub comments: Vec<ContextComment>,
    pub comments_total: u32,
    pub comments_truncated: bool,

    pub code_hints: Vec<ContextCodeHint>,
    pub code_hints_total: u32,
    pub code_hints_truncated: bool,

    /// How many artifacts the query matched. This is the INTERNAL scoping
    /// step behind the `comments` + `code_hints` lanes (they are scoped to
    /// these artifacts plus the recalled memories), surfaced as a count so
    /// that scoping isn't a black box — deliberately NOT a fifth lane.
    pub artifacts_matched: u32,
    /// At least one item was dropped by the CHAR budget rather than by an
    /// item cap.
    pub budget_exceeded: bool,
    /// Chars the returned items charged against the budget.
    pub chars: u32,
    /// D7/SL3c: open asks + unacknowledged hands + live takes + contested
    /// takes on the CALLER's own slate (derived from `memory_project`, the
    /// `memory-<slug>` passthrough — invariant #10 MS amendment). Not a
    /// sixth lane ("one home per action", D7): it feeds `scent`'s fifth
    /// count and nothing else. Zero when `memory_project` is absent, does
    /// not resolve to a slate, or the slate does not exist — the pack must
    /// never fail because of the slate.
    pub slate_items: u32,
    pub ms: u64,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Char-safe truncation (never a byte split, never a silent ellipsis).
fn clip(s: &str, max: usize) -> String {
    if chars(s) <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Flatten to ONE line: the pack is a scan surface, and a digest excerpt with
/// embedded newlines would blow every lane's char accounting apart from its
/// rendered size.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- the pure budget core --------------------------------------------------

/// What [`fit_lane`] decided, per lane.
#[derive(Debug)]
struct LaneFit<T> {
    kept: Vec<T>,
    /// The returned list is shorter than the input — for ANY reason.
    truncated: bool,
    /// …and at least one of those drops was the CHAR budget, not the item cap.
    budget_dropped: bool,
    chars: usize,
}

/// The item cap + hard char budget, applied in that order over an
/// already-ordered lane. Pure, total, and the ONLY place a pack item is ever
/// dropped — so "never silent" is a property of one function, not a habit
/// spread across four lanes.
///
/// A single item bigger than the whole lane budget keeps NOTHING and reports
/// `budget_dropped` (no "at least one" carve-out: a carve-out would make the
/// budget soft, and a soft budget is the thing the caller cannot reason
/// about).
fn fit_lane<T>(
    items: Vec<T>,
    item_cap: usize,
    char_budget: usize,
    cost: impl Fn(&T) -> usize,
) -> LaneFit<T> {
    let total = items.len();
    let mut kept: Vec<T> = Vec::new();
    let mut used = 0usize;
    let mut budget_dropped = false;
    for (i, item) in items.into_iter().enumerate() {
        if i >= item_cap {
            break;
        }
        let c = cost(&item);
        if used + c > char_budget {
            budget_dropped = true;
            break;
        }
        used += c;
        kept.push(item);
    }
    LaneFit {
        truncated: kept.len() < total,
        budget_dropped,
        chars: used,
        kept,
    }
}

/// A lane's slice of the char budget. Integer math, floor semantics — the
/// shares sum to 100 so the sum of the lane budgets is `budget` (± the
/// rounding the floors discard, which is at most 3 chars).
fn lane_budget(budget: u32, share_pct: u32) -> usize {
    (budget as usize).saturating_mul(share_pct as usize) / 100
}

/// The COUNTS line. This is the entire payload the `UserPromptSubmit` hook
/// injects on turn 1 — so it must be countable, honest, and carry no
/// substance whatsoever (that is what keeps R0/R3 intact without a
/// re-ruling). Counts are the PRE-truncation totals: telling the agent "5
/// memories" when the budget only rendered 2 is the honest number to act on,
/// since acting means running `kb context` with a bigger budget.
///
/// `slate_items` (D7, SL3c): the caller's slate contributes a FIFTH count,
/// not a fifth lane ("one home per action") — `part` renders it exactly
/// like the other four, so it drops out entirely at zero and every
/// pre-existing golden string above stays byte-identical.
fn scent_line(
    sessions: u32,
    comments: u32,
    memories: u32,
    code_hints: u32,
    slate_items: u32,
) -> String {
    fn part(n: u32, one: &str, many: &str) -> Option<String> {
        if n == 0 {
            None
        } else if n == 1 {
            Some(format!("1 {one}"))
        } else {
            Some(format!("{n} {many}"))
        }
    }
    let parts: Vec<String> = [
        part(sessions, "prior session", "prior sessions"),
        part(comments, "open comment", "open comments"),
        part(memories, "memory", "memories"),
        part(code_hints, "cited code path", "cited code paths"),
        part(slate_items, "slate item in play", "slate items in play"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        "no prior context".to_string()
    } else {
        parts.join(" · ")
    }
}

/// D7/SL3c: `open asks + unacknowledged hands + live takes + contested
/// takes` on the CALLER's own slate — `scent_line`'s fifth count.
///
/// Which slate: `memory_project` already carries `memory-<slug>` (invariant
/// #10 MS amendment, the same param `kb context`'s CLI wrapper derives from
/// the git main-checkout basename); this strips the prefix and validates
/// the remainder as a [`kb_core::slate::SlateSlug`]. Absent, malformed, or
/// naming a slate that has never been posted to all resolve to 0 — never an
/// error, because a scent line must never fail the whole pack over a lane
/// nothing else in this route touches.
///
/// Reuses `routes::slates`'s own ledger loader and presence slice (rather
/// than a second reader) so this count can never disagree with the board's
/// — same posts, same [`kb_core::sessions::live::LivePolicy`], same
/// `all: true` unbudgeted projection the board's own counts read
/// (`routes::slates::facts_opts`, mirrored here since it takes no
/// dependency this route doesn't already have).
async fn slate_items_in_play(state: &Arc<KbHandles>, memory_project: Option<&str>) -> u32 {
    let Some(slug) = memory_project
        .and_then(|p| p.strip_prefix("memory-"))
        .filter(|s| !s.is_empty())
        .and_then(|s| kb_core::slate::SlateSlug::new(s).ok())
    else {
        return 0;
    };
    let paths = state.paths.clone();
    if !paths.slate_ledger_file(&slug).exists() {
        return 0;
    }
    let now_unix = chrono::Utc::now().timestamp();
    let presence = crate::routes::slates::presence_slice(state, now_unix);

    let load_slug = slug.clone();
    let posts = match tokio::task::spawn_blocking(move || {
        crate::routes::slates::load_posts(&paths, &load_slug)
    })
    .await
    {
        Ok(Ok(posts)) => posts,
        // A read error or a join error is not this pack's problem to
        // surface — the same "degrade to empty rather than 500" posture
        // every other lane above takes on its own composed read.
        _ => return 0,
    };

    let policy = kb_core::sessions::live::LivePolicy::default();
    let opts = kb_core::slate::ProjectOpts {
        all: true,
        slug: slug.to_string(),
        ..kb_core::slate::ProjectOpts::default()
    };
    let d = kb_core::slate::project(&posts, now_unix, &policy, &presence, &opts);

    let mut n = d.ask_total + d.hand_total;
    for t in &d.sections.take {
        if t.liveness == Some(kb_core::slate::Liveness::Live) {
            n += 1;
        }
        if t.contested {
            n += 1;
        }
    }
    n as u32
}

/// STABLE partition: same-cwd sessions first, everything else after, each
/// group keeping `recollect`'s own deterministic order. Not a re-rank — the
/// score is untouched and `same_cwd` is surfaced on every row, so a caller
/// can always see why a row floated.
fn partition_by_cwd(sessions: Vec<ContextSession>) -> Vec<ContextSession> {
    let (same, other): (Vec<_>, Vec<_>) = sessions.into_iter().partition(|s| s.same_cwd);
    same.into_iter().chain(other).collect()
}

/// Does this session belong to the caller's `?cwd=`? Mirrors `recollect`'s
/// own `--folder` rule, which accepts EITHER form: the session's stored
/// `folder` is the cwd's basename, but a caller may legitimately pass (and a
/// row may legitimately store) the full path.
fn session_matches_cwd(cwd: Option<&str>, folder: Option<&str>) -> bool {
    let (Some(cwd), Some(folder)) = (cwd, folder) else {
        return false;
    };
    if folder == cwd {
        return true;
    }
    std::path::Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        == Some(folder)
}

// ---- the handler -----------------------------------------------------------

/// `GET /api/context?q=&cwd=&budget=&session=&memory_project=&memory_visible_to=`
/// — the pack. `memory_project`/`memory_visible_to` (CT-B2, memory-scoping)
/// narrow the memories lane's `recall_compose` call (`scope` stays `"all"`)
/// exactly as the recall route's own `project`/`visible_to` params narrow a
/// direct recall (kb-cli derives both from the caller's repo slug — there
/// is no standalone `--visible-to` flag);
/// both are optional passthroughs, byte-identical to before when absent.
pub async fn get(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Query(params): Query<ContextParams>,
) -> Response {
    let started = std::time::Instant::now();
    let q = params.q.trim().to_string();
    if q.is_empty() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "q is required (the task text the pack is assembled for)".into(),
        ));
    }
    let budget = params
        .budget
        .unwrap_or(CONTEXT_DEFAULT_BUDGET)
        .clamp(CONTEXT_MIN_BUDGET, CONTEXT_MAX_BUDGET);
    let cwd = params
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let session = params
        .session
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // The three query-bearing lanes run SEQUENTIALLY on purpose: they embed
    // the SAME text, and `state.embed_cache` is a plain LRU with no in-flight
    // dedup — running them concurrently would pay the embed three times over
    // (a self-inflicted stampede) to save one cache-warm hop.

    // --- lane 1: memories (invariant #10) ---------------------------------
    let recall = match crate::routes::memory::recall_compose(
        Arc::clone(&state),
        identity,
        crate::routes::memory::Params {
            q: q.clone(),
            scope: "all".to_string(),
            project: params.memory_project.clone(),
            limit: Some(MEMORY_POOL),
            for_kb: None,
            visible_to: params.memory_visible_to.clone(),
            no_floor: params.no_floor,
            with_weekly: false,
        },
    )
    .await
    {
        Ok(r) => r,
        // Unreachable: the only Err arm is an unsupported `scope`, and this
        // call site hardcodes "all". Degrade to an empty lane rather than
        // 500-ing the whole pack over a lane that can't fail.
        Err(_) => crate::routes::memory::RecallResponse {
            hits: Vec::new(),
            ms: 0,
        },
    };
    let memory_ids: Vec<(String, String)> = recall
        .hits
        .iter()
        .map(|h| (h.kb.clone(), h.id.clone()))
        .collect();
    let memories: Vec<ContextMemory> = recall
        .hits
        .into_iter()
        .map(|h| ContextMemory {
            id: h.id,
            kb: h.kb,
            title: one_line(&h.title),
            source_relative: h.source_relative,
            summary: h
                .summary
                .as_deref()
                .map(|s| clip(&one_line(s), MEMORY_SUMMARY_CHARS))
                .filter(|s| !s.is_empty()),
            score: h.score,
            salience: h.salience,
            rank: h.rank,
            rel: h.rel,
            decay: h.decay,
            age_days: h.age_days,
            relevance_factor: h.relevance_factor,
            stability: h.stability,
            flagged: h.flagged,
            warns: h.warns,
            drift_open: h.drift_open,
            global: h.global,
            linked_kbs: h.linked_kbs,
        })
        .collect();

    // --- lane 2: session POINTERS (invariant #11 R0/R1/R3) ----------------
    let recollect = crate::routes::sessions::recollect_compose(
        Arc::clone(&state),
        crate::routes::sessions::RecollectParams {
            q: Some(q.clone()),
            similar_to: None,
            // NOT a server-side folder filter — see `ContextParams::cwd`.
            folder: None,
            project: None,
            since: None,
            limit: Some(SESSION_POOL),
        },
    )
    .await
    .map(|r| r.sessions)
    // Unreachable for the same reason as recall's arm above (this call site
    // always passes exactly one of q/similar_to).
    .unwrap_or_default();
    let sessions: Vec<ContextSession> = recollect
        .into_iter()
        // #11 multi-capture: the caller's own session accrues `sessions` rows
        // mid-flight. Telling an agent about itself is noise, not context.
        .filter(|s| Some(&s.session_id) != session.as_ref())
        .map(|s| {
            let excerpt = s
                .summary
                .as_deref()
                .or(s.outcome.as_deref())
                .map(|t| clip(&one_line(t), SESSION_EXCERPT_CHARS))
                .filter(|t| !t.is_empty());
            ContextSession {
                same_cwd: session_matches_cwd(cwd.as_deref(), s.folder.as_deref()),
                session_id: s.session_id,
                kb: s.kb,
                display_name: one_line(&s.display_name),
                started_at: s.started_at,
                age_days: s.age_days,
                stale: s.stale,
                commit_count: s.commit_count,
                error_count: s.error_count,
                excerpt,
            }
        })
        .collect();
    let sessions = partition_by_cwd(sessions);

    // --- the internal artifact-match step (scopes lanes 3 + 4) ------------
    let matched = matching_artifacts(&state, &q).await;
    let artifacts_matched = matched.len() as u32;

    // --- lane 3: open comments on matching artifacts (invariant #6) -------
    let mut scope_ids: HashSet<(String, String)> = matched.iter().cloned().collect();
    scope_ids.extend(memory_ids.iter().cloned());
    let comments = collect_scoped_comments(&state, &scope_ids).await;

    // --- lane 4: the kb-local code_refs summary (invariant #2) ------------
    // Sources = the matched artifacts AND the recalled memories. Memories'
    // own `code_hints` are per-hit capped (CT-C4's CODE_HINTS_CAP); this lane
    // re-reads them so the fleet summary isn't quietly built on a truncation.
    let mut hint_sources: Vec<(String, String)> = matched;
    for m in &memory_ids {
        if !hint_sources.contains(m) {
            hint_sources.push(m.clone());
        }
    }
    let code_hints = collect_code_hints(&state, &hint_sources).await;

    // --- budget ------------------------------------------------------------
    let memories_total = memories.len() as u32;
    let sessions_total = sessions.len() as u32;
    let comments_total = comments.len() as u32;
    let code_hints_total = code_hints.len() as u32;

    let m = fit_lane(
        memories,
        MEMORY_ITEM_CAP,
        lane_budget(budget, LANE_SHARE_MEMORIES),
        ContextMemory::cost,
    );
    let s = fit_lane(
        sessions,
        SESSION_ITEM_CAP,
        lane_budget(budget, LANE_SHARE_SESSIONS),
        ContextSession::cost,
    );
    let c = fit_lane(
        comments,
        COMMENT_ITEM_CAP,
        lane_budget(budget, LANE_SHARE_COMMENTS),
        ContextComment::cost,
    );
    let h = fit_lane(
        code_hints,
        CODE_HINT_ITEM_CAP,
        lane_budget(budget, LANE_SHARE_CODE_HINTS),
        ContextCodeHint::cost,
    );

    let slate_items = slate_items_in_play(&state, params.memory_project.as_deref()).await;

    let scent = scent_line(
        sessions_total,
        comments_total,
        memories_total,
        code_hints_total,
        slate_items,
    );
    Json(ContextResponse {
        q,
        cwd,
        session,
        budget,
        no_floor: params.no_floor,
        scent,
        memories_truncated: m.truncated,
        memories: m.kept,
        memories_total,
        sessions_truncated: s.truncated,
        sessions: s.kept,
        sessions_total,
        comments_truncated: c.truncated,
        comments: c.kept,
        comments_total,
        code_hints_truncated: h.truncated,
        code_hints: h.kept,
        code_hints_total,
        artifacts_matched,
        budget_exceeded: m.budget_dropped
            || s.budget_dropped
            || c.budget_dropped
            || h.budget_dropped,
        chars: (m.chars + s.chars + c.chars + h.chars) as u32,
        slate_items,
        ms: started.elapsed().as_millis() as u64,
    })
    .into_response()
}

// ---- the composed reads ----------------------------------------------------

/// The internal artifact-match step: which ARTIFACTS does this query match?
/// Used only to SCOPE the comments + code_hints lanes — without it the
/// comments lane degenerates into the fleet inbox, which returns the same
/// number every turn and is therefore a nag rather than a scent.
///
/// Two exclusions, both load-bearing:
/// * memory-scoped corpora — their docs ARE the memories lane (#10);
/// * `memory-session` docs — R0 keeps transcripts out of default search, and
///   the sessions lane already covers them as pointers (#11).
///
/// Per-corpus futures through `buffered_join` (#28); a corpus that fails its
/// index check or query contributes an empty partial rather than 500-ing.
async fn matching_artifacts(state: &Arc<KbHandles>, q: &str) -> Vec<(String, String)> {
    // Embed once per distinct embedder MODEL (mirrors recall/recollect —
    // keying on model NAME, not dim: two models can share a dim and feeding
    // one's vector to the other's index returns garbage).
    let mut vec_by_model: std::collections::HashMap<&'static str, Vec<f32>> =
        std::collections::HashMap::new();
    for (_, ctx) in state.kbs.iter() {
        if ctx.memory_scope.is_some() {
            continue;
        }
        if let Some(emb) = &ctx.embedder {
            // Invariant #15: the guard yields a `&'static str` and drops
            // before the await below.
            let model = emb.lock().unwrap_or_else(|e| e.into_inner()).model_name();
            if let std::collections::hash_map::Entry::Vacant(slot) = vec_by_model.entry(model) {
                if let Ok(out) = crate::embed_cache::embed_query(&state.embed_cache, emb, q).await {
                    slot.insert(out.vec);
                }
            }
        }
    }

    let vbm = &vec_by_model;
    let mut futs: Vec<super::CorpusFut<'_, Vec<(String, String)>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        if ctx.memory_scope.is_some() {
            continue;
        }
        futs.push(Box::pin(async move {
            if let Err(e) = ctx.storage.ensure_fts_index().await {
                tracing::warn!(kb = %kb_name, error = %e, "context: ensure_fts_index failed; skipping corpus");
                return Vec::new();
            }
            let model_vec = ctx.embedder.as_ref().and_then(|emb| {
                let m = emb.lock().unwrap_or_else(|e| e.into_inner()).model_name();
                vbm.get(m).cloned()
            });
            let rows = match model_vec {
                Some(v) => {
                    ctx.storage
                        .hybrid_query(q.to_string(), v, ARTIFACT_MATCH_POOL)
                        .await
                }
                None => {
                    ctx.storage
                        .bm25_query(q.to_string(), ARTIFACT_MATCH_POOL, false)
                        .await
                }
            };
            let rows = rows.unwrap_or_else(|e| {
                tracing::warn!(kb = %kb_name, error = %e, "context: artifact match query failed");
                Vec::new()
            });
            rows.into_iter()
                .filter(|d| {
                    d.kb_category.as_deref() != Some(kb_core::sessions::MEMORY_SESSION_CATEGORY)
                })
                .take(ARTIFACT_MATCH_CAP)
                .map(|d| (kb_name.as_str().to_string(), d.id))
                .collect::<Vec<_>>()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// OPEN comments, scoped to `scope_ids`, through the ONE canonical collector
/// (`routes::inbox::collect_open` — `review::load`, never a hand-rolled JSON
/// parse, invariant #6). Only corpora that actually hold a scoped id are
/// walked.
///
/// `[kb-flag]` / `[kb-drift]` comments are EXCLUDED: they are already
/// surfaced, per memory, as `flagged` / `drift_open` on the memories lane
/// (CT-C1 / CT-C4). Emitting them here too would double-report the same
/// marker in one pack and inflate the scent line the hook injects.
async fn collect_scoped_comments(
    state: &Arc<KbHandles>,
    scope_ids: &HashSet<(String, String)>,
) -> Vec<ContextComment> {
    let kbs_in_scope: BTreeSet<&str> = scope_ids.iter().map(|(kb, _)| kb.as_str()).collect();
    let mut futs: Vec<super::CorpusFut<'_, Vec<crate::routes::inbox::InboxItem>>> = Vec::new();
    for (kb_name, ctx) in state.kbs.iter() {
        if !kbs_in_scope.contains(kb_name.as_str()) {
            continue;
        }
        let review_dir = state.paths.kb_review_dir(kb_name);
        futs.push(Box::pin(async move {
            crate::routes::inbox::collect_open(kb_name.as_str(), ctx, &review_dir).await
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let mut items: Vec<ContextComment> = super::buffered_join(futs, state.fanout_cap)
        .await
        .into_iter()
        .flatten()
        .filter(|it| scope_ids.contains(&(it.kb.clone(), it.artifact_id.clone())))
        // `InboxItem::excerpt` is the body's leading chars, so the two
        // prefix predicates read correctly off it.
        .filter(|it| {
            !kb_core::memory::is_flag_comment(&it.excerpt)
                && !kb_core::memory::is_drift_comment(&it.excerpt)
        })
        .map(|it| ContextComment {
            kb: it.kb,
            artifact_id: it.artifact_id,
            source_relative: it.source_relative,
            title: one_line(&it.title),
            comment_id: it.comment_id,
            excerpt: clip(&one_line(&it.excerpt), COMMENT_EXCERPT_CHARS),
            author: it.author,
            reply_count: it.reply_count,
            stale: it.stale,
            updated_at: it.updated_at,
        })
        .collect();
    // Newest activity first, with a TOTAL-order tiebreak so a same-second
    // batch is stable across requests (the inbox route's own rule).
    items.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.artifact_id.cmp(&b.artifact_id))
            .then_with(|| a.comment_id.cmp(&b.comment_id))
    });
    items
}

/// The kb-LOCAL `code_refs` summary: distinct path-shaped hints across the
/// pack's own artifacts, with how many of them cite each path. ONE
/// `code_refs_of` read per source id (bounded by `ARTIFACT_MATCH_CAP ×
/// corpora + MEMORY_ITEM_CAP`), never a kb-code call — invariant #2 again:
/// kb has no tree and no symbols, so these are hints and nothing else.
async fn collect_code_hints(
    state: &Arc<KbHandles>,
    sources: &[(String, String)],
) -> Vec<ContextCodeHint> {
    // BTreeMap so equal-count paths come out in a stable, total order.
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for (kb, id) in sources {
        let Ok(kb_name) = kb_core::types::KbName::new(kb) else {
            continue;
        };
        let Some(ctx) = state.kbs.get(&kb_name) else {
            continue;
        };
        let doc = match ctx.storage.code_refs_of(id.clone()).await {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(kb = %kb, id = %id, error = %e, "context: code_refs_of failed; artifact contributes no hints");
                continue;
            }
        };
        // Distinct WITHIN a doc first, so a doc citing one path ten times
        // counts once toward `cited_by` (the unit is CITING DOCS).
        let mut seen: HashSet<&str> = HashSet::new();
        for r in &doc.refs {
            if !crate::routes::memory::is_path_shaped_kind(&r.kind) {
                continue;
            }
            let Some(hint) = r.path_hint.as_deref() else {
                continue;
            };
            if !seen.insert(hint) {
                continue;
            }
            *counts.entry(hint.to_string()).or_insert(0) += 1;
        }
    }
    let mut out: Vec<ContextCodeHint> = counts
        .into_iter()
        .map(|(path_hint, cited_by)| ContextCodeHint {
            path_hint,
            cited_by,
        })
        .collect();
    // Most-cited first; path ascending breaks every tie (total order).
    out.sort_by(|a, b| {
        b.cited_by
            .cmp(&a.cited_by)
            .then_with(|| a.path_hint.cmp(&b.path_hint))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the scent line (what the hook injects on turn 1) ----------------

    #[test]
    fn scent_line_reports_honest_zeros_on_an_empty_corpus() {
        assert_eq!(scent_line(0, 0, 0, 0, 0), "no prior context");
    }

    #[test]
    fn scent_line_matches_the_ratified_example_shape() {
        assert_eq!(
            scent_line(3, 2, 5, 0, 0),
            "3 prior sessions · 2 open comments · 5 memories"
        );
    }

    #[test]
    fn scent_line_singularises_every_lane() {
        assert_eq!(
            scent_line(1, 1, 1, 1, 1),
            "1 prior session · 1 open comment · 1 memory · 1 cited code path · 1 slate item in play"
        );
    }

    #[test]
    fn scent_line_omits_empty_lanes_entirely() {
        assert_eq!(scent_line(0, 0, 4, 0, 0), "4 memories");
        assert_eq!(
            scent_line(2, 0, 0, 7, 0),
            "2 prior sessions · 7 cited code paths"
        );
    }

    /// SL3c (D7): a nonzero slate count renders as the fifth, plural
    /// `slate items in play` segment, joined the same way as every other
    /// lane — and stays omitted when zero (already covered above).
    #[test]
    fn scent_line_reports_slate_items_as_a_fifth_count() {
        assert_eq!(scent_line(0, 0, 0, 0, 3), "3 slate items in play");
        assert_eq!(
            scent_line(3, 2, 5, 0, 4),
            "3 prior sessions · 2 open comments · 5 memories · 4 slate items in play"
        );
    }

    // ---- the budget ------------------------------------------------------

    fn hint(path: &str) -> ContextCodeHint {
        ContextCodeHint {
            path_hint: path.to_string(),
            cited_by: 1,
        }
    }

    #[test]
    fn fit_lane_keeps_everything_that_fits() {
        let fit = fit_lane(
            vec![hint("aaaa"), hint("bbbb")],
            10,
            100,
            ContextCodeHint::cost,
        );
        assert_eq!(fit.kept.len(), 2);
        assert!(!fit.truncated);
        assert!(!fit.budget_dropped);
        assert_eq!(fit.chars, 8);
    }

    #[test]
    fn fit_lane_item_cap_truncation_is_explicit_and_not_a_budget_drop() {
        let fit = fit_lane(
            vec![hint("a"), hint("b"), hint("c")],
            2,
            1_000,
            ContextCodeHint::cost,
        );
        assert_eq!(fit.kept.len(), 2);
        assert!(fit.truncated, "the caller must be able to see the drop");
        assert!(
            !fit.budget_dropped,
            "an item-cap drop must not be blamed on the char budget"
        );
    }

    #[test]
    fn fit_lane_char_budget_truncation_is_explicit() {
        // costs 4, 4, 4 — a 9-char budget fits exactly two.
        let fit = fit_lane(
            vec![hint("aaaa"), hint("bbbb"), hint("cccc")],
            10,
            9,
            ContextCodeHint::cost,
        );
        assert_eq!(fit.kept.len(), 2);
        assert!(fit.truncated);
        assert!(fit.budget_dropped);
        assert_eq!(fit.chars, 8);
    }

    #[test]
    fn fit_lane_budget_is_hard_even_when_it_fits_nothing() {
        // No "always keep one" carve-out: a soft budget is one the caller
        // cannot reason about.
        let fit = fit_lane(vec![hint("aaaaaaaaaa")], 10, 3, ContextCodeHint::cost);
        assert!(fit.kept.is_empty());
        assert!(fit.truncated);
        assert!(fit.budget_dropped);
        assert_eq!(fit.chars, 0);
    }

    #[test]
    fn fit_lane_on_an_empty_lane_reports_no_truncation() {
        let fit = fit_lane(Vec::<ContextCodeHint>::new(), 5, 100, ContextCodeHint::cost);
        assert!(fit.kept.is_empty());
        assert!(!fit.truncated);
        assert!(!fit.budget_dropped);
        assert_eq!(fit.chars, 0);
    }

    #[test]
    fn lane_shares_sum_to_the_whole_budget() {
        assert_eq!(
            LANE_SHARE_MEMORIES + LANE_SHARE_SESSIONS + LANE_SHARE_COMMENTS + LANE_SHARE_CODE_HINTS,
            100
        );
        let b = CONTEXT_DEFAULT_BUDGET;
        let sum = lane_budget(b, LANE_SHARE_MEMORIES)
            + lane_budget(b, LANE_SHARE_SESSIONS)
            + lane_budget(b, LANE_SHARE_COMMENTS)
            + lane_budget(b, LANE_SHARE_CODE_HINTS);
        assert_eq!(sum, b as usize);
    }

    #[test]
    fn lanes_are_independent_of_each_other() {
        // A lane's contents are a pure function of its OWN items + its OWN
        // share — no redistribution, so a huge memories lane can never
        // starve the code-hints lane.
        let small = fit_lane(
            vec![hint("aaaa"), hint("bbbb")],
            10,
            lane_budget(CONTEXT_DEFAULT_BUDGET, LANE_SHARE_CODE_HINTS),
            ContextCodeHint::cost,
        );
        let same_again = fit_lane(
            vec![hint("aaaa"), hint("bbbb")],
            10,
            lane_budget(CONTEXT_DEFAULT_BUDGET, LANE_SHARE_CODE_HINTS),
            ContextCodeHint::cost,
        );
        assert_eq!(small.kept.len(), same_again.kept.len());
        assert_eq!(small.chars, same_again.chars);
    }

    // ---- the cwd partition ------------------------------------------------

    fn sess(id: &str, same_cwd: bool) -> ContextSession {
        ContextSession {
            session_id: id.to_string(),
            kb: "sessions".to_string(),
            display_name: id.to_string(),
            started_at: 0,
            age_days: 0,
            stale: false,
            commit_count: 0,
            error_count: 0,
            excerpt: None,
            same_cwd,
        }
    }

    #[test]
    fn partition_by_cwd_is_stable_within_each_group() {
        let out = partition_by_cwd(vec![
            sess("a", false),
            sess("b", true),
            sess("c", false),
            sess("d", true),
        ]);
        let ids: Vec<&str> = out.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["b", "d", "a", "c"]);
    }

    #[test]
    fn partition_by_cwd_is_a_no_op_without_a_cwd() {
        let out = partition_by_cwd(vec![sess("a", false), sess("b", false)]);
        let ids: Vec<&str> = out.iter().map(|s| s.session_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn session_matches_cwd_on_full_path_and_on_basename() {
        assert!(session_matches_cwd(
            Some("/home/user/project/kb"),
            Some("kb")
        ));
        assert!(session_matches_cwd(
            Some("/home/user/project/kb"),
            Some("/home/user/project/kb")
        ));
        assert!(!session_matches_cwd(
            Some("/home/user/project/kb"),
            Some("demo-repo")
        ));
        assert!(!session_matches_cwd(None, Some("kb")));
        assert!(!session_matches_cwd(Some("/home/user/project/kb"), None));
    }

    // ---- text shaping -----------------------------------------------------

    #[test]
    fn one_line_flattens_newlines_so_char_cost_matches_rendered_size() {
        assert_eq!(one_line("a\nb   c\t d"), "a b c d");
    }

    #[test]
    fn clip_never_splits_a_multibyte_char() {
        // 5 chars, 10 bytes — clipping to 3 must yield 3 CHARS.
        let s = "ααβββ";
        assert_eq!(clip(s, 3).chars().count(), 3);
        assert_eq!(clip(s, 99), s);
    }

    // ---- CT-B2 — `memory_project`/`memory_visible_to` passthrough --------
    //
    // Full-daemon fixture, mirroring the `l7_recall_*`/CT-B2 style in
    // `routes::memory::tests` (same reasoning for living here rather than
    // in `tests/end_to_end.rs`: that file isn't in this task's owned set).

    fn ctxb2_kb_section(path: std::path::PathBuf) -> kb_core::config::KbSection {
        kb_core::config::KbSection {
            path,
            skip_patterns: Vec::new(),
            ui: kb_core::config::UiSection::default(),
            embedding_model: None,
            reranker_model: None,
            chunked_embeddings: false,
            graph_boost: None,
            outbound: None,
            atlas: None,
            templates: BTreeMap::new(),
            memory_scope: Some("global".to_string()),
            default_search_category: None,
            code_url: None,
            decay_policy: None,
            versions: None,
            reading_progress: None,
            search: Default::default(),
            indexable_extensions: None,
            reconcile_secs: None,
            capture_dir: None,
            resurface: None,
            slo: None,
        }
    }

    /// Boot a daemon with one `global`-scope memory corpus holding a
    /// single, entirely UNLINKED memory (no `kb-global`/`kb-linked-kbs`
    /// meta at all — see `routes::memory::tests` for why that requires
    /// writing the file directly rather than going through `POST
    /// …/artifacts`).
    async fn ctxb2_boot_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
        let tmp = tempfile::tempdir().unwrap();
        let mem = tmp.path().join("mem");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(
            mem.join("note.html"),
            r#"<html><head>
                <meta name="kb-category" content="memory-user">
                <title>Context Probe Note</title>
            </head><body><p>xenoncontext passthrough probe</p></body></html>"#,
        )
        .unwrap();

        let daemon_name = format!(
            "ctxb2-{}",
            tmp.path().file_name().unwrap().to_string_lossy()
        );
        let mut kb_map: BTreeMap<kb_core::types::KbName, kb_core::config::KbSection> =
            BTreeMap::new();
        kb_map.insert(
            kb_core::types::KbName::new("ctxmem").unwrap(),
            ctxb2_kb_section(mem),
        );
        let cfg = kb_core::config::KbConfig {
            daemon: kb_core::config::DaemonSection {
                name: Some(daemon_name.clone()),
            },
            defaults: kb_core::config::DefaultsSection {
                embedding_model: None,
                disable_embedder_fallback: true,
            },
            kb: kb_map,
            ..Default::default()
        };
        let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), daemon_name);
        let (addr, _task) = crate::serve_on_random_port_with_paths(cfg, paths)
            .await
            .expect("serve");

        // Poll until the memory has landed before returning.
        let client = reqwest::Client::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let docs: Vec<serde_json::Value> = client
                .get(format!("http://{addr}/api/kb/ctxmem/docs?limit=50"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap_or_default();
            if docs
                .iter()
                .any(|d| d["title"].as_str() == Some("Context Probe Note"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for the context-probe memory to be indexed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        (tmp, addr)
    }

    /// Absent `memory_project`/`memory_visible_to` must be byte-identical
    /// to the pre-CT-B2 shape. On a fixture with a single unlinked memory
    /// (nothing for `visible_to` to filter, and only one project corpus
    /// for `memory_project` to narrow to), a request omitting both params
    /// and a request naming them explicitly (but harmlessly — a
    /// `memory_project` equal to the memory's own kb, and a
    /// `memory_visible_to` naming an unrelated kb) must produce the exact
    /// same `memories` lane.
    #[tokio::test]
    async fn context_memory_project_and_visible_to_absent_is_byte_identical() {
        let (_tmp, addr) = ctxb2_boot_fixture().await;
        let client = reqwest::Client::new();

        let without_params: serde_json::Value = client
            .get(format!(
                "http://{addr}/api/context?q=xenoncontext&budget=4000"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let with_params: serde_json::Value = client
            .get(format!(
                "http://{addr}/api/context?q=xenoncontext&budget=4000&memory_project=ctxmem&memory_visible_to=some-unrelated-kb"
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        // The contract is "same CODE PATH", not "time stands still": each
        // request recomputes age_days (and its decay/score derivatives)
        // from wall-clock NOW, so two requests straddling a whole-second
        // boundary legitimately differ in the last float ulp — the exact
        // flake nextest's parallel load surfaced (PF-C1 flip). Strip the
        // three clock-derived per-hit fields, then require byte-identity
        // on everything that CAN be identical.
        let strip_clock = |lane: &serde_json::Value| -> serde_json::Value {
            let mut lane = lane.clone();
            if let Some(hits) = lane.as_array_mut() {
                for h in hits {
                    if let Some(obj) = h.as_object_mut() {
                        for k in ["age_days", "decay", "score"] {
                            obj.remove(k);
                        }
                    }
                }
            }
            lane
        };
        assert_eq!(
            strip_clock(&without_params["memories"]),
            strip_clock(&with_params["memories"]),
            "absent memory_project/memory_visible_to must be byte-identical \
             to a harmless explicit pair (clock-derived fields excepted)"
        );
        // Sanity: the probe memory actually made it into the lane — an
        // empty-vs-empty comparison would pass vacuously.
        assert_eq!(
            without_params["memories"].as_array().unwrap().len(),
            1,
            "expected exactly the one seeded memory in the lane"
        );
    }
}
